//! Deterministic verification wired into the workflow.
//!
//! The plan declares WHICH canonical layers must pass; this module
//! owns the deterministic mapping from layer names to commands and
//! executes them through the governed tool runtime with Verifier
//! capabilities (read + allowlisted cargo only). There is no path
//! here that runs commands outside capability/allowlist checks.
//!
//! Loop budget: every failed suite is durable evidence; once
//! MAX_FIX_ROUNDS suites have failed, the run fails terminally
//! instead of looping forever (ADR-007).

use hephaestus_agent::role::{AgentRole, RoleManifest};
use hephaestus_core::Error;
use hephaestus_core::domain::StrategyNotes;
use hephaestus_core::id::{ExecutionId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_db::Db;
use hephaestus_verify::plan::{LayerSpec, VerificationPlan};
use hephaestus_verify::report::run_plan;

use crate::analysis::{StageError, WorkspaceLayout, chain_job};
use crate::execution::MAX_FIX_ROUNDS;
use crate::jobs::{JobPayload, Queue, RepairCause};
use crate::worker::{HandlerOutcome, JobHandler};

/// Canonical layer names plans may require today. Widening this set
/// is a reviewed-code change: each entry pins a concrete command.
pub const SUPPORTED_LAYERS: &[&str] = &["format", "lint", "typecheck", "unit_tests"];

/// Failure to translate a required layer into a runnable command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationLayerError {
    /// Layer name outside the supported set.
    Unsupported {
        /// The rejected name.
        name: String,
    },
    /// The same canonical layer appeared twice.
    Duplicate {
        /// The repeated name.
        name: String,
    },
}

impl std::fmt::Display for VerificationLayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerificationLayerError::Unsupported { name } => {
                write!(
                    f,
                    "unsupported verification layer {name:?}; supported: {SUPPORTED_LAYERS:?}"
                )
            }
            VerificationLayerError::Duplicate { name } => {
                write!(f, "duplicate verification layer {name:?}")
            }
        }
    }
}

impl std::error::Error for VerificationLayerError {}

/// Map one canonical layer name to its pinned command.
pub fn layer_spec_for(name: &str) -> Result<LayerSpec, VerificationLayerError> {
    match name {
        "format" => Ok(LayerSpec::new(
            hephaestus_verify::plan::Layer::Format,
            "cargo",
            &["fmt", "--all", "--", "--check"],
        )),
        "lint" => Ok(LayerSpec::new(
            hephaestus_verify::plan::Layer::Lint,
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        )),
        "typecheck" => Ok(LayerSpec::new(
            hephaestus_verify::plan::Layer::TypeCheck,
            "cargo",
            &["check", "--workspace"],
        )),
        "unit_tests" => Ok(LayerSpec::new(
            hephaestus_verify::plan::Layer::UnitTests,
            "cargo",
            &["test", "--workspace"],
        )),
        other => Err(VerificationLayerError::Unsupported {
            name: other.to_string(),
        }),
    }
}

/// Build the executable plan from a strategy's required layers.
/// An empty requirement list falls back to the deterministic default
/// set - mirrors how role manifests ship Rust-oriented defaults that
/// operators may narrow, never widen.
pub fn default_layer_plan() -> VerificationPlan {
    let specs: std::result::Result<Vec<LayerSpec>, VerificationLayerError> =
        ["format", "lint", "unit_tests"]
            .iter()
            .map(|n| layer_spec_for(n))
            .collect();
    match specs.and_then(|specs| {
        VerificationPlan::new(specs).map_err(|e| VerificationLayerError::Unsupported {
            name: e.to_string(),
        })
    }) {
        Ok(plan) => plan,
        // The default set is compile-time known: non-empty, unique,
        // supported. Reaching this arm is a programmer error.
        Err(e) => unreachable!("built-in default verification plan rejected itself: {e}"),
    }
}

/// Build the executable plan a strategy requires.
///
/// Unknown or duplicated layer names fail closed: verification never
/// silently drops a layer the planner demanded.
pub fn layer_plan_from_strategy(
    strategy: &StrategyNotes,
) -> Result<VerificationPlan, VerificationLayerError> {
    if strategy.verification.is_empty() {
        return Ok(default_layer_plan());
    }
    let mut specs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in &strategy.verification {
        if !seen.insert(name.as_str()) {
            return Err(VerificationLayerError::Duplicate { name: name.clone() });
        }
        specs.push(layer_spec_for(name)?);
    }
    VerificationPlan::new(specs).map_err(|e| VerificationLayerError::Unsupported {
        name: e.to_string(),
    })
}

/// Handler for the verification stage of an execution.
pub struct RunVerificationHandler {
    layout: WorkspaceLayout,
}

impl RunVerificationHandler {
    /// Bind the workspace layout.
    pub fn new(layout: WorkspaceLayout) -> Self {
        Self { layout }
    }
}

async fn run_verification(
    db: &Db,
    layout: &WorkspaceLayout,
    execution_id: ExecutionId,
    run_id: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    let scope = db.run_scope(run_id).await.map_err(StageError::Permanent)?;
    let exec = db
        .get_execution(scope.organization_id, execution_id)
        .await
        .map_err(StageError::Permanent)?;
    if exec.run_id != run_id.as_uuid() || exec.task_id != scope.task_id.as_uuid() {
        return Err(StageError::Permanent(Error::Validation {
            field: "payload".into(),
            message: "execution does not belong to the driving run".into(),
        }));
    }
    if exec.status != "active" {
        return Ok(()); // stale redelivery after a terminal verdict
    }

    // Implementing -> Verifying. Redeliveries already in Verifying
    // proceed: suites are deterministic and evidence is append-only.
    // A run already in Reviewing means an earlier delivery finished
    // the suite and nominated review - there is nothing left to
    // record, and re-running would duplicate durable evidence rows.
    if scope.state == WorkflowState::Implementing {
        db.transition_run(
            scope.organization_id,
            run_id,
            WorkflowState::Implementing,
            TransitionEvent::StartVerification,
            "verification-handler",
        )
        .await
        .map_err(StageError::Retryable)?;
    } else if scope.state == WorkflowState::Reviewing {
        return Ok(()); // stale redelivery after the pass transition
    } else if scope.state != WorkflowState::Verifying {
        return Err(StageError::Permanent(Error::Conflict {
            message: format!(
                "run is {}, not implementing, verifying, or reviewing",
                scope.state.name()
            ),
        }));
    }

    let plan = db
        .get_current_plan(scope.organization_id, scope.task_id)
        .await
        .map_err(StageError::Permanent)?
        .ok_or_else(|| StageError::Permanent(Error::NotFound { entity: "plan" }))?;
    if plan.id.as_uuid() != exec.plan_id {
        return Err(StageError::Permanent(Error::Validation {
            field: "plan".into(),
            message: "current plan does not match the execution's plan".into(),
        }));
    }

    let vplan = layer_plan_from_strategy(&plan.strategy).map_err(|e| {
        // Misconfigured required layers are a data problem, not noise.
        StageError::Permanent(Error::Validation {
            field: "strategy.verification".into(),
            message: e.to_string(),
        })
    })?;

    let workspace = layout.run_repo_dir(run_id);
    let caps = RoleManifest::built_in(AgentRole::Verifier).capability_set(workspace);
    let report = run_plan(&vplan, &caps, false);
    let passed = report.all_passed();

    let report_json = serde_json::to_value(&report)
        .map_err(|e| Error::Storage(Box::new(e)))
        .map_err(StageError::Permanent)?;
    db.record_verification(
        scope.organization_id,
        execution_id,
        run_id,
        passed,
        &report_json,
    )
    .await
    .map_err(StageError::Permanent)?;

    if passed {
        // Verifying -> Reviewing BEFORE nominating review. The review
        // handler refuses every ingress state but Reviewing, and the
        // state machine - not this handler - owns the hop; skipping it
        // would strand a passed suite one state short of reviewable.
        // The redelivery that arrives after this move is absorbed by
        // the guard above, so the CAS here always sees Verifying.
        db.transition_run(
            scope.organization_id,
            run_id,
            WorkflowState::Verifying,
            TransitionEvent::VerificationPassed,
            "verification-handler",
        )
        .await
        .map_err(StageError::Retryable)?;
        // Review decides the next hop; verification only nominates.
        // The key carries the suite cycle so later rounds after a
        // repair are not swallowed by this round's idempotent job.
        let cycle = db
            .latest_verification(scope.organization_id, execution_id)
            .await
            .map_err(StageError::Permanent)?
            .map(|v| v.cycle)
            .unwrap_or(0);
        chain_job(
            db,
            Queue::Review.as_str(),
            &format!("run-review:{cycle}:{run_id}"),
            run_id,
            JobPayload::RunReview {
                execution_id,
                run_id,
            },
        )
        .await
        .map_err(StageError::Retryable)?;
        return Ok(());
    }

    let failed = db
        .count_failed_verifications(scope.organization_id, execution_id)
        .await
        .map_err(StageError::Permanent)?;
    if failed >= MAX_FIX_ROUNDS {
        db.finish_execution(scope.organization_id, execution_id, "failed")
            .await
            .map_err(StageError::Permanent)?;
        if let Err(e) = db
            .transition_run(
                scope.organization_id,
                run_id,
                WorkflowState::Verifying,
                TransitionEvent::Fail,
                "verification-handler",
            )
            .await
            && !matches!(e, Error::Conflict { .. })
        {
            return Err(StageError::Retryable(e));
        }
        return Err(StageError::Permanent(Error::BudgetExhausted {
            what: "fix-rounds",
        }));
    }

    db.transition_run(
        scope.organization_id,
        run_id,
        WorkflowState::Verifying,
        TransitionEvent::VerificationFailed,
        "verification-handler",
    )
    .await
    .map_err(StageError::Retryable)?;
    chain_job(
        db,
        Queue::Implementation.as_str(),
        &format!("repair:{failed}:{run_id}"),
        run_id,
        JobPayload::RepairExecution {
            execution_id,
            run_id,
            cause: RepairCause::Verification,
        },
    )
    .await
    .map_err(StageError::Retryable)?;
    Ok(())
}

impl JobHandler for RunVerificationHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::RunVerification {
                execution_id,
                run_id,
            } = payload
            else {
                tracing::error!(?payload, "verification handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match run_verification(&db, &self.layout, execution_id, run_id).await {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(run = %run_id, error = %e, "verification retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(run = %run_id, error = %e, "verification permanent failure");
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
    use hephaestus_core::domain::StrategyNotes;

    #[test]
    fn every_supported_layer_maps_to_a_pinned_command() {
        for name in SUPPORTED_LAYERS {
            let spec = layer_spec_for(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(spec.program, "cargo");
            assert!(!spec.args.is_empty());
        }
    }

    #[test]
    fn unknown_layers_are_rejected_loudly() {
        let err = layer_spec_for("security").expect_err("unsupported must fail");
        assert!(err.to_string().contains("unsupported"));
        let err2 = layer_spec_for("").expect_err("empty must fail");
        assert!(matches!(err2, VerificationLayerError::Unsupported { .. }));
    }

    #[test]
    fn strategy_layers_build_a_unique_ordered_plan() {
        let strategy = StrategyNotes {
            rollback: None,
            deployment: None,
            verification: vec!["format".into(), "unit_tests".into()],
        };
        let plan = layer_plan_from_strategy(&strategy).expect("valid strategy");
        assert_eq!(plan.layers().len(), 2);
        assert_eq!(plan.layers()[0].layer.as_str(), "format");

        let dup = StrategyNotes {
            rollback: None,
            deployment: None,
            verification: vec!["lint".into(), "lint".into()],
        };
        let err = layer_plan_from_strategy(&dup).expect_err("duplicates refused");
        assert!(matches!(err, VerificationLayerError::Duplicate { .. }));
    }

    #[test]
    fn empty_strategy_falls_back_to_the_default_set() {
        let strategy = StrategyNotes::default();
        let plan = layer_plan_from_strategy(&strategy).expect("default plan");
        assert!(!plan.layers().is_empty());
        let names: Vec<_> = plan.layers().iter().map(|l| l.layer.as_str()).collect();
        for n in names {
            assert!(
                SUPPORTED_LAYERS.contains(&n),
                "default layers must themselves be supported"
            );
        }
    }
}
