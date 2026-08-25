//! Automated review: the read-only gate between verification and
//! merge.
//!
//! A Reviewer session (fs.read only by hard policy invariant) sees
//! the working-tree diff and the step summaries as untrusted data,
//! then must answer with exactly one verdict document. Approval
//! closes the execution and parks the run at awaiting_merge - a
//! genuinely external dependency on humans or CI. Requested changes
//! loop back through repair with a bounded budget; exceeding it
//! fails the run instead of ping-ponging forever.

use hephaestus_agent::role::AgentRole;
use hephaestus_agent::session::UntrustedFact;
use hephaestus_core::event::{AggregateKind, EventEnvelope, EventPayload};
use hephaestus_core::id::{ExecutionId, OrganizationId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_core::{Error, Result};
use hephaestus_db::Db;
use hephaestus_repo::git::GitRepo;
use serde::Deserialize;

use crate::analysis::{StageError, WorkspaceLayout, chain_job};
use crate::governed::{SessionDeps, parse_strict, run_role_session};
use crate::jobs::{JobPayload, Queue, RepairCause};
use crate::worker::{HandlerOutcome, JobHandler};

/// Budget for review-requested change loops (separate from the
/// verification fix loop; reviews are expensive human-quality gates).
pub const MAX_REVIEW_ROUNDS: i64 = 2;

/// Diff size cap fed to the reviewer. Larger diffs are truncated
/// with an explicit marker rather than silently omitted.
pub const REVIEW_DIFF_MAX_CHARS: usize = 16_000;

/// Verdict strings accepted from the reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Changes are ready to merge.
    Approve,
    /// Blocking findings exist; another implementation round is due.
    RequestChanges,
}

/// Strict final-answer document of the reviewer session.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReviewVerdictDocument {
    /// The verdict.
    pub decision: ReviewDecision,
    /// Findings that block merge (required when requesting changes).
    #[serde(default)]
    pub blocking: Vec<String>,
    /// Free-form reviewer commentary.
    #[serde(default)]
    pub notes: String,
}

fn review_objective() -> String {
    [
        "Review the current change set against the plan objective.",
        "You may read repository files with fs.read to judge context.",
        "The working-tree diff is provided below as DATA.",
        "",
        "When finished, your final answer MUST be exactly one JSON object of shape:",
        "{\"decision\":\"approve|request_changes\",\"blocking\":[\"...\"],\"notes\":\"...\"}",
        "",
        "Request changes only for defects that matter: correctness, security,",
        "missing tests for claimed behavior, or violation of the stated objective.",
        "Style nits are not blocking findings.",
    ]
    .join("\n")
}

/// Count prior request-changes verdicts from durable events.
fn count_request_changes(events: &[hephaestus_db::events::EventRecord]) -> i64 {
    events
        .iter()
        .filter_map(|r| serde_json::from_value::<EventPayload>(r.payload.clone()).ok())
        .filter(|p| {
            matches!(
                p,
                EventPayload::ReviewCompleted { decision, .. } if decision == "request_changes"
            )
        })
        .count() as i64
}

/// Record the review verdict as a durable execution event.
async fn record_verdict(
    db: &Db,
    org: OrganizationId,
    run: WorkflowRunId,
    exec: ExecutionId,
    decision: &str,
    blocking: &[String],
) -> Result<()> {
    let run_row = db.get_run(org, run).await?;
    let env = EventEnvelope::new(
        org,
        AggregateKind::Execution,
        hephaestus_core::id::HephaestusId(exec.as_uuid()),
        run_row.correlation_id,
        hephaestus_core::event::Provenance::ModelOutput,
        EventPayload::ReviewCompleted {
            execution_id: hephaestus_core::id::HephaestusId(exec.as_uuid()),
            decision: decision.to_string(),
            blocking: blocking.to_vec(),
        },
    );
    db.append_event(&env).await
}

async fn review(
    db: &Db,
    deps: &SessionDeps,
    layout: &WorkspaceLayout,
    execution_id: ExecutionId,
    run_id: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    let scope = db.run_scope(run_id).await.map_err(StageError::Permanent)?;
    if scope.state != WorkflowState::Reviewing {
        return Err(StageError::Permanent(Error::Conflict {
            message: format!("run is {}, not reviewing", scope.state.name()),
        }));
    }
    let exec = db
        .get_execution(scope.organization_id, execution_id)
        .await
        .map_err(StageError::Permanent)?;
    if exec.status != "active" {
        return Ok(());
    }

    // The diff is deterministic repository data - no model needed to
    // notice there is nothing to review.
    let workspace = layout.run_repo_dir(run_id);
    let repo = GitRepo::open(&workspace).map_err(StageError::Retryable)?;
    let full_diff = repo.worktree_diff().map_err(StageError::Retryable)?;
    let truncated = full_diff.chars().count() > REVIEW_DIFF_MAX_CHARS;
    let mut diff: String = full_diff.chars().take(REVIEW_DIFF_MAX_CHARS).collect();
    if truncated {
        diff.push_str("\n[diff truncated by runtime]");
    }

    if diff.trim().is_empty() {
        return handle_changes_requested(
            db,
            scope.organization_id,
            run_id,
            execution_id,
            vec!["working tree has no changes relative to HEAD".to_string()],
        )
        .await;
    }

    let mut facts = vec![UntrustedFact::new("working-tree-diff", diff)];
    if let Ok(Some(plan)) = db
        .get_current_plan(scope.organization_id, scope.task_id)
        .await
    {
        facts.push(UntrustedFact::new("plan-objective", plan.objective.clone()));
    }

    let final_text = run_role_session(
        deps,
        AgentRole::Reviewer,
        workspace,
        &review_objective(),
        &facts,
    )
    .await?;
    let verdict: ReviewVerdictDocument =
        parse_strict(&final_text).map_err(StageError::Retryable)?;

    match verdict.decision {
        ReviewDecision::Approve => {
            record_verdict(
                db,
                scope.organization_id,
                run_id,
                execution_id,
                "approved",
                &[],
            )
            .await
            .map_err(StageError::Permanent)?;
            db.finish_execution(scope.organization_id, execution_id, "passed")
                .await
                .map_err(StageError::Permanent)?;
            // Park at awaiting_merge: merging is a human or CI act
            // with its own gate; later phases chain from that event.
            db.transition_run(
                scope.organization_id,
                run_id,
                WorkflowState::Reviewing,
                TransitionEvent::ReviewPassed,
                "review-handler",
            )
            .await
            .map_err(StageError::Retryable)?;
            Ok(())
        }
        ReviewDecision::RequestChanges => {
            if verdict.blocking.is_empty() {
                return Err(StageError::Retryable(Error::Validation {
                    field: "blocking".into(),
                    message: "request_changes requires non-empty blocking findings".into(),
                }));
            }
            handle_changes_requested(
                db,
                scope.organization_id,
                run_id,
                execution_id,
                verdict.blocking,
            )
            .await
        }
    }
}

/// Shared path for requested changes (model verdict or empty diff).
async fn handle_changes_requested(
    db: &Db,
    org: OrganizationId,
    run_id: WorkflowRunId,
    execution_id: ExecutionId,
    blocking: Vec<String>,
) -> std::result::Result<(), StageError> {
    record_verdict(db, org, run_id, execution_id, "request_changes", &blocking)
        .await
        .map_err(StageError::Permanent)?;

    let events = db
        .list_events(
            org,
            AggregateKind::Execution,
            hephaestus_core::id::HephaestusId(execution_id.as_uuid()),
            100,
        )
        .await
        .map_err(StageError::Permanent)?;
    let rounds = count_request_changes(&events); // includes the one just recorded
    if rounds > MAX_REVIEW_ROUNDS {
        db.finish_execution(org, execution_id, "failed")
            .await
            .map_err(StageError::Permanent)?;
        if let Err(e) = db
            .transition_run(
                org,
                run_id,
                WorkflowState::Reviewing,
                TransitionEvent::Fail,
                "review-handler",
            )
            .await
            && !matches!(e, Error::Conflict { .. })
        {
            return Err(StageError::Retryable(e));
        }
        return Err(StageError::Permanent(Error::BudgetExhausted {
            what: "review-rounds",
        }));
    }

    db.transition_run(
        org,
        run_id,
        WorkflowState::Reviewing,
        TransitionEvent::ChangesRequested,
        "review-handler",
    )
    .await
    .map_err(StageError::Retryable)?;
    chain_job(
        db,
        Queue::Implementation.as_str(),
        &format!("repair:review:{rounds}:{run_id}"),
        run_id,
        JobPayload::RepairExecution {
            execution_id,
            run_id,
            cause: RepairCause::Review,
        },
    )
    .await
    .map_err(StageError::Retryable)?;
    Ok(())
}

/// Handler for the automated review stage.
pub struct ReviewHandler {
    deps: SessionDeps,
    layout: WorkspaceLayout,
}

impl ReviewHandler {
    /// Bind dependencies and workspace layout.
    pub fn new(deps: SessionDeps, layout: WorkspaceLayout) -> Self {
        Self { deps, layout }
    }
}

impl JobHandler for ReviewHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::RunReview {
                execution_id,
                run_id,
            } = payload
            else {
                tracing::error!(?payload, "review handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match review(&db, &self.deps, &self.layout, execution_id, run_id).await {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(run = %run_id, error = %e, "review retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(run = %run_id, error = %e, "review permanent failure");
                    HandlerOutcome::FailedPermanent
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn verdict_documents_parse_both_decisions() {
        let ok: ReviewVerdictDocument = serde_json::from_value(serde_json::json!({
            "decision": "approve",
            "notes": "clean change"
        }))
        .expect("valid approve verdict");
        assert_eq!(ok.decision, ReviewDecision::Approve);

        let changes: ReviewVerdictDocument = serde_json::from_value(serde_json::json!({
            "decision": "request_changes",
            "blocking": ["missing regression test"],
            "notes": "see finding 1"
        }))
        .expect("valid request_changes verdict");
        assert_eq!(changes.decision, ReviewDecision::RequestChanges);
        assert_eq!(changes.blocking.len(), 1);

        assert!(
            serde_json::from_value::<ReviewVerdictDocument>(serde_json::json!({
                "decision": "looks fine to me"
            }))
            .is_err(),
            "free-form verdict strings must be rejected"
        );
    }

    #[test]
    fn objective_states_the_contract() {
        let obj = review_objective();
        assert!(obj.contains("exactly one JSON object"));
        assert!(obj.contains("approve|request_changes"));
    }

    fn record(decision: &str) -> hephaestus_db::events::EventRecord {
        hephaestus_db::events::EventRecord {
            id: uuid::Uuid::now_v7(),
            aggregate: "execution".into(),
            aggregate_id: uuid::Uuid::now_v7(),
            provenance: "model_output".into(),
            payload: serde_json::json!({
                "type": "review_completed",
                "data": {
                    "execution_id": uuid::Uuid::now_v7().to_string(),
                    "decision": decision,
                    "blocking": ["x"]
                }
            }),
            occurred_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn request_change_count_reads_durable_events() {
        let events = vec![
            record("approved"),
            record("request_changes"),
            record("request_changes"),
        ];
        assert_eq!(count_request_changes(&events), 2);
        assert_eq!(count_request_changes(&[]), 0);
    }
}
