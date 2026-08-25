//! Governed implementation of approved plans.
//!
//! Two handlers share one shape:
//!
//! * "ExecuteStep" runs a governed Implementer session against the
//!   next pending plan step, then chains either the following step or
//!   the verification suite when the plan is exhausted.
//! * "RepairExecution" runs the same role against verification or
//!   review findings. Every repair chain is created by the stage that
//!   found the problem, so at most one is in flight per run.
//!
//! Final answers must be exactly one JSON document ("status",
//! "summary", "files_changed"); anything else retries within queue
//! budgets. A blocked implementation is terminal for the run: the
//! reason is persisted and the workflow fails loudly - nothing is
//! guessed at.

use hephaestus_agent::role::AgentRole;
use hephaestus_agent::session::UntrustedFact;
use hephaestus_core::Error;
use hephaestus_core::domain::RequirementKind;
use hephaestus_core::id::{ExecutionId, OrganizationId, PlanId, StepId, TaskId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_db::Db;
use hephaestus_db::execution::ExecutionRow;
use serde::Deserialize;

use crate::analysis::{StageError, WorkspaceLayout, chain_job};
use crate::governed::{SessionDeps, parse_strict, run_role_session};
use crate::jobs::{JobPayload, Queue, RepairCause};
use crate::worker::{HandlerOutcome, JobHandler};

/// Upper bound on failed verification suites before the fix loop
/// terminates the run (ADR-007: the loop budget lives in this layer).
pub const MAX_FIX_ROUNDS: i64 = 3;

/// Cap for one evidence excerpt fed to a repair session.
pub(crate) const EVIDENCE_EXCERPT_CHARS: usize = 1_200;

/// Outcome status strings accepted in implementation documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// The work is done.
    Completed,
    /// The implementer cannot proceed; a human must intervene.
    Blocked,
}

/// Strict final-answer document of implementation sessions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StepOutcomeDocument {
    /// What happened.
    pub status: StepStatus,
    /// One-paragraph account of what changed, or what blocks progress.
    pub summary: String,
    /// Workspace-relative paths touched (evidence trail; advisory -
    /// the filesystem itself stays authoritative).
    #[serde(default)]
    pub files_changed: Vec<String>,
}

/// Alias kept for API readability: repairs produce the same document.
pub type ImplementationDocument = StepOutcomeDocument;

fn kind_label(k: RequirementKind) -> &'static str {
    match k {
        RequirementKind::Explicit => "explicit",
        RequirementKind::Inferred => "inferred",
        RequirementKind::Assumption => "assumption",
        RequirementKind::Unknown => "unknown",
    }
}

fn implementation_objective(step_action: &str, step_verification: &str) -> String {
    [
        "Implement ONE approved plan step inside the workspace.",
        "You may read files with fs.read, write files with fs.write and run allowlisted",
        "commands with shell.exec. Stay strictly inside the given step.",
        "",
        "When finished, your final answer MUST be exactly one JSON object of shape:",
        "{\"status\":\"completed|blocked\",\"summary\":\"...\",\"files_changed\":[\"path\"]}",
        "",
        "Use status=blocked only when you genuinely cannot proceed; describe precisely",
        "what information or access is missing. Never claim completion without doing",
        "the work. The step's own verification reference follows:",
    ]
    .join("\n")
        + "\n"
        + step_verification
        + "\n\nSTEP:\n"
        + step_action
}

fn repair_objective(cause: RepairCause) -> String {
    let found_by = match cause {
        RepairCause::Verification => {
            "Deterministic verification failed; the failing layers and their output follow."
        }
        RepairCause::Review => {
            "The automated reviewer requested changes; the blocking findings follow."
        }
    };
    [
        "Repair the current workspace state so the pending problems go away.",
        "Do not start unrelated work; change only what the findings require.",
        "You may read and write files and run allowlisted commands.",
        "",
        found_by,
        "",
        "When finished, your final answer MUST be exactly one JSON object of shape:",
        "{\"status\":\"completed|blocked\",\"summary\":\"...\",\"files_changed\":[\"path\"]}",
    ]
    .join("\n")
}

/// Shared guards for implementation-stage jobs: run must be in
/// implementing state, identifiers must agree with stored rows.
async fn guard(
    db: &Db,
    execution_id: ExecutionId,
    run_id: WorkflowRunId,
    plan_hint: Option<PlanId>,
) -> std::result::Result<(OrganizationId, TaskId, ExecutionRow), StageError> {
    let scope = db.run_scope(run_id).await.map_err(StageError::Permanent)?;
    if scope.state != WorkflowState::Implementing {
        return Err(StageError::Permanent(Error::Conflict {
            message: format!(
                "run is {}, not implementing; refusing to execute work out of order",
                scope.state.name()
            ),
        }));
    }
    let exec = db
        .get_execution(scope.organization_id, execution_id)
        .await
        .map_err(StageError::Permanent)?;
    // Payload integrity: identifiers must agree with stored rows.
    if exec.run_id != run_id.as_uuid() || exec.task_id != scope.task_id.as_uuid() {
        return Err(StageError::Permanent(Error::Validation {
            field: "payload".into(),
            message: "execution does not belong to the driving run".into(),
        }));
    }
    if let Some(plan_id) = plan_hint
        && exec.plan_id != plan_id.as_uuid()
    {
        return Err(StageError::Permanent(Error::Validation {
            field: "payload".into(),
            message: "plan id does not match the execution's plan".into(),
        }));
    }
    Ok((scope.organization_id, scope.task_id, exec))
}

/// Run one implementation session and file its outcome. Returns the
/// parsed document when the implementer finished coherently.
async fn run_implementer(
    deps: &SessionDeps,
    layout: &WorkspaceLayout,
    task_id: TaskId,
    run_id: WorkflowRunId,
    objective: String,
    facts: Vec<UntrustedFact>,
) -> std::result::Result<StepOutcomeDocument, StageError> {
    let _ = task_id; // context facts already carry the requirement text
    let workspace = layout.run_repo_dir(run_id);
    let final_text =
        run_role_session(deps, AgentRole::Implementer, workspace, &objective, &facts).await?;
    let doc: StepOutcomeDocument = parse_strict(&final_text).map_err(StageError::Retryable)?;
    if doc.summary.trim().is_empty() {
        return Err(StageError::Retryable(Error::Validation {
            field: "summary".into(),
            message: "implementation document requires a non-empty summary".into(),
        }));
    }
    Ok(doc)
}

/// Terminal handling for blocked work: persist the reason on the
/// execution, close it as failed and fail the workflow run loudly.
async fn fail_run_blocked(
    db: &Db,
    org: OrganizationId,
    run_id: WorkflowRunId,
    exec: ExecutionId,
    step_hint: Option<StepId>,
    summary: &str,
) -> std::result::Result<(), StageError> {
    if let Some(step) = step_hint {
        let _ = db.fail_execution_step(org, exec, step, summary).await;
    }
    db.finish_execution(org, exec, "failed")
        .await
        .map_err(StageError::Permanent)?;
    // Legal from implementing; CAS conflict means someone failed the
    // run concurrently - fine either way.
    if let Err(e) = db
        .transition_run(
            org,
            run_id,
            WorkflowState::Implementing,
            TransitionEvent::Fail,
            "execution-handler",
        )
        .await
        && !matches!(e, Error::Conflict { .. })
    {
        return Err(StageError::Retryable(e));
    }
    Ok(())
}
async fn execute_step(
    db: &Db,
    deps: &SessionDeps,
    layout: &WorkspaceLayout,
    execution_id: ExecutionId,
    step_id: StepId,
    plan_id: PlanId,
    run_id: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    let (org, task_id, exec) = guard(db, execution_id, run_id, Some(plan_id)).await?;
    if exec.status != "active" {
        // Stale redelivery after the loop reached a terminal verdict.
        return Ok(());
    }

    let steps = db
        .list_execution_steps(org, execution_id)
        .await
        .map_err(StageError::Permanent)?;
    let Some(step_row) = steps.iter().find(|s| s.step_id == step_id.as_uuid()) else {
        return Err(StageError::Permanent(Error::NotFound {
            entity: "execution_step",
        }));
    };
    if step_row.status == "completed" {
        return Ok(()); // idempotent redelivery
    }

    // Context facts: requirements, plan goal, prior step outcomes,
    // and the current step itself. All framed untrusted downstream.
    let mut facts = Vec::new();
    if let Ok(requirements) = db.list_requirements(org, task_id).await {
        for (i, r) in requirements.iter().enumerate() {
            facts.push(UntrustedFact::new(
                format!("requirement-{}", i + 1),
                format!("[{}] {}", kind_label(r.kind), r.statement),
            ));
        }
    }
    if let Ok(Some(plan)) = db.get_current_plan(org, task_id).await
        && plan.id.as_uuid() == plan_id.as_uuid()
    {
        facts.push(UntrustedFact::new("plan-objective", plan.objective.clone()));
    }
    for done in steps.iter().filter(|s| s.status == "completed") {
        facts.push(UntrustedFact::new(
            format!("step-{}-outcome", done.position),
            format!("{}: {}", done.action, done.summary),
        ));
    }
    facts.push(UntrustedFact::new(
        "current-step",
        format!(
            "position {} of {}: {}",
            step_row.position,
            steps.len(),
            step_row.action
        ),
    ));

    let doc = run_implementer(
        deps,
        layout,
        task_id,
        run_id,
        implementation_objective(&step_row.action, &step_row.verification),
        facts,
    )
    .await?;

    match doc.status {
        StepStatus::Blocked => {
            fail_run_blocked(db, org, run_id, execution_id, Some(step_id), &doc.summary).await?;
            Err(StageError::Permanent(Error::Validation {
                field: "status".into(),
                message: format!("implementation reported blocked: {}", doc.summary),
            }))
        }
        StepStatus::Completed => {
            db.complete_execution_step(
                org,
                execution_id,
                step_id,
                &doc.summary,
                &doc.files_changed,
            )
            .await
            .map_err(StageError::Retryable)?;

            match db
                .next_pending_step(org, execution_id)
                .await
                .map_err(StageError::Permanent)?
            {
                Some(next) => {
                    chain_job(
                        db,
                        Queue::Implementation.as_str(),
                        &format!("execute-step:{}:{}", next.step_id, run_id),
                        run_id,
                        JobPayload::ExecuteStep {
                            execution_id,
                            step_id: StepId::from_uuid(next.step_id),
                            plan_id,
                            run_id,
                        },
                    )
                    .await
                    .map_err(StageError::Retryable)?;
                }
                None => {
                    chain_job(
                        db,
                        Queue::Verification.as_str(),
                        &format!("run-verification:1:{run_id}"),
                        run_id,
                        JobPayload::RunVerification {
                            execution_id,
                            run_id,
                        },
                    )
                    .await
                    .map_err(StageError::Retryable)?;
                }
            }
            Ok(())
        }
    }
}

/// Build repair-session facts from the newest verification report.
async fn verification_facts(
    db: &Db,
    org: OrganizationId,
    exec: ExecutionId,
) -> Result<Vec<UntrustedFact>, StageError> {
    let latest = db
        .latest_verification(org, exec)
        .await
        .map_err(StageError::Permanent)?
        .ok_or_else(|| {
            StageError::Permanent(Error::Validation {
                field: "verification".into(),
                message: "repair requested but no verification evidence exists".into(),
            })
        })?;
    let mut content = format!("verification cycle {} failed:", latest.cycle);
    if let Some(layers) = latest.report.get("results").and_then(|r| r.as_array()) {
        for layer in layers
            .iter()
            .filter(|l| l.get("passed").and_then(|p| p.as_bool()) != Some(true))
        {
            let name = layer
                .get("layer")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let exit = layer
                .get("exit_code")
                .map(|v| v.to_string())
                .unwrap_or_default();
            let excerpt = layer
                .get("output_excerpt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cut: String = excerpt.chars().take(EVIDENCE_EXCERPT_CHARS).collect();
            content.push_str(&format!("\nlayer={name} exit={exit}\noutput:\n{cut}\n"));
        }
    } else {
        let pretty = serde_json::to_string_pretty(&latest.report).unwrap_or_default();
        content.push_str(&pretty);
    }
    Ok(vec![UntrustedFact::new("verification-report", content)])
}

/// Build repair-session facts from the newest review verdict event.
async fn review_facts(
    db: &Db,
    org: OrganizationId,
    exec: ExecutionId,
) -> Result<Vec<UntrustedFact>, StageError> {
    use hephaestus_core::event::{AggregateKind, EventPayload};
    let events = db
        .list_events(
            org,
            AggregateKind::Execution,
            hephaestus_core::id::HephaestusId(exec.as_uuid()),
            50,
        )
        .await
        .map_err(StageError::Permanent)?;
    // Records store pre-serialized typed payloads; newest wins.
    for record in events.iter().rev() {
        if let Ok(EventPayload::ReviewCompleted { blocking, .. }) =
            serde_json::from_value::<EventPayload>(record.payload.clone())
        {
            let content = format!(
                "blocking findings:\n{}",
                blocking
                    .iter()
                    .enumerate()
                    .map(|(i, b)| format!("{}. {}", i + 1, b))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            return Ok(vec![UntrustedFact::new("review-findings", content)]);
        }
    }
    Err(StageError::Permanent(Error::Validation {
        field: "review".into(),
        message: "repair requested but no review verdict event exists".into(),
    }))
}

async fn repair(
    db: &Db,
    deps: &SessionDeps,
    layout: &WorkspaceLayout,
    execution_id: ExecutionId,
    run_id: WorkflowRunId,
    cause: RepairCause,
) -> std::result::Result<(), StageError> {
    let (org, task_id, exec) = guard(db, execution_id, run_id, None).await?;
    if exec.status != "active" {
        return Ok(());
    }

    let mut facts = match cause {
        RepairCause::Verification => verification_facts(db, org, execution_id).await?,
        RepairCause::Review => review_facts(db, org, execution_id).await?,
    };
    if let Ok(Some(plan)) = db.get_current_plan(org, task_id).await {
        facts.push(UntrustedFact::new("plan-objective", plan.objective.clone()));
    }

    let doc = run_implementer(
        deps,
        layout,
        task_id,
        run_id,
        repair_objective(cause),
        facts,
    )
    .await?;

    match doc.status {
        StepStatus::Blocked => {
            fail_run_blocked(db, org, run_id, execution_id, None, &doc.summary).await?;
            Err(StageError::Permanent(Error::Validation {
                field: "status".into(),
                message: format!("repair reported blocked: {}", doc.summary),
            }))
        }
        StepStatus::Completed => {
            // Deterministic next cycle keeps the enqueue key stable
            // across crash-redelivery windows.
            let next_cycle = db
                .latest_verification(org, execution_id)
                .await
                .map_err(StageError::Permanent)?
                .map(|v| v.cycle + 1)
                .unwrap_or(1);
            chain_job(
                db,
                Queue::Verification.as_str(),
                &format!("run-verification:{next_cycle}:{run_id}"),
                run_id,
                JobPayload::RunVerification {
                    execution_id,
                    run_id,
                },
            )
            .await
            .map_err(StageError::Retryable)?;
            Ok(())
        }
    }
}

/// Handler for one-step implementation jobs.
pub struct ExecutionHandler {
    deps: SessionDeps,
    layout: WorkspaceLayout,
}

impl ExecutionHandler {
    /// Bind dependencies and workspace layout.
    pub fn new(deps: SessionDeps, layout: WorkspaceLayout) -> Self {
        Self { deps, layout }
    }
}

impl JobHandler for ExecutionHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::ExecuteStep {
                execution_id,
                step_id,
                plan_id,
                run_id,
            } = payload
            else {
                tracing::error!(?payload, "execution handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match execute_step(
                &db,
                &self.deps,
                &self.layout,
                execution_id,
                step_id,
                plan_id,
                run_id,
            )
            .await
            {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(run = %run_id, error = %e, "step execution retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(run = %run_id, error = %e, "step execution permanent failure");
                    HandlerOutcome::FailedPermanent
                }
            }
        })
    }
}

/// Handler for repair-round jobs (verification or review findings).
pub struct RepairHandler {
    deps: SessionDeps,
    layout: WorkspaceLayout,
}

impl RepairHandler {
    /// Bind dependencies and workspace layout.
    pub fn new(deps: SessionDeps, layout: WorkspaceLayout) -> Self {
        Self { deps, layout }
    }
}

impl JobHandler for RepairHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::RepairExecution {
                execution_id,
                run_id,
                cause,
            } = payload
            else {
                tracing::error!(?payload, "repair handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match repair(&db, &self.deps, &self.layout, execution_id, run_id, cause).await {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(run = %run_id, error = %e, "repair retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(run = %run_id, error = %e, "repair permanent failure");
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
    fn outcome_document_parses_both_statuses() {
        let ok: StepOutcomeDocument = serde_json::from_value(serde_json::json!({
            "status": "completed",
            "summary": "added guard and regression test",
            "files_changed": ["src/user.rs", "tests/user.rs"]
        }))
        .expect("valid completed document");
        assert_eq!(ok.status, StepStatus::Completed);
        assert_eq!(ok.files_changed.len(), 2);

        let blocked: StepOutcomeDocument = serde_json::from_value(serde_json::json!({
            "status": "blocked",
            "summary": "credentials for staging are missing"
        }))
        .expect("valid blocked document");
        assert_eq!(blocked.status, StepStatus::Blocked);
        assert!(blocked.files_changed.is_empty(), "default applies");

        assert!(
            serde_json::from_value::<StepOutcomeDocument>(serde_json::json!({
                "status": "done_somehow", "summary": "x"
            }))
            .is_err(),
            "unknown status strings must be rejected"
        );
    }

    #[test]
    fn objectives_name_the_contract_and_the_step() {
        let obj = implementation_objective("Add None guard in UserService.find", "unit:none_guard");
        assert!(obj.contains("exactly one JSON object"));
        assert!(obj.contains("unit:none_guard"));
        assert!(obj.contains("Add None guard in UserService.find"));

        let rep = repair_objective(RepairCause::Review);
        assert!(rep.contains("reviewer requested changes"));
        let rep2 = repair_objective(RepairCause::Verification);
        assert!(rep2.contains("verification failed"));
    }
}
