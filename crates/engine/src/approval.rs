//! The plan approval gate: the only door from planning into
//! execution.
//!
//! A decision is recorded on the open gate transactionally, then the
//! workflow advances through a legal transition. Approval bootstraps
//! an execution over the current plan and enqueues its first step;
//! rejection returns the run to planning and chains plan generation
//! again so the planner can revise. Nothing here bypasses the state
//! machine or the queue.
//!
//! Recovery contract (ADR-007): every durable step of this service
//! is idempotent, so a crash mid-decision is recovered by re-issuing
//! the same decision - the service detects already-applied state and
//! returns the same outcome instead of double-spending it.

use hephaestus_core::id::{ExecutionId, OrganizationId, StepId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_core::{Error, Result};
use hephaestus_db::Db;

use crate::analysis::{WorkspaceLayout, chain_job};
use crate::governed::SessionDeps;
use crate::jobs::{JobPayload, Queue};

/// A human decision at the plan approval gate.
#[derive(Debug, Clone)]
pub struct ApprovalDecisionInput {
    /// Who decided (authenticated principal). Recorded in the event
    /// log; never inferred.
    pub approver: String,
    /// Approve or reject the current plan.
    pub approved: bool,
    /// Free-form rationale, persisted with the decision.
    pub reason: Option<String>,
}

/// What happened after a decision was durably applied.
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalOutcome {
    /// Plan approved: an active execution covers it.
    Approved {
        /// The active execution for this run.
        execution_id: ExecutionId,
        /// The step enqueued for implementation, when one was still
        /// pending at decision time.
        enqueued_step: Option<StepId>,
    },
    /// Plan rejected: the run returned to planning and plan
    /// generation was chained again.
    Rejected {
        /// The gate row carrying the decision (round identity).
        approval_id: hephaestus_core::id::HephaestusId,
    },
}

/// Decides plan-approval gates and boots the execution pipeline.
#[derive(Clone)]
pub struct ApprovalService {
    // Reserved for the replan chain; the decision path itself only
    // needs durable storage. Workers populate both via [`Self::new`];
    // decision-only surfaces such as the HTTP API construct the
    // service through [`Self::gate_only`] because they deliberately
    // hold no model-provider credentials.
    #[allow(dead_code)]
    deps: Option<SessionDeps>,
    #[allow(dead_code)]
    layout: Option<WorkspaceLayout>,
}

impl ApprovalService {
    /// Bind dependencies and workspace layout.
    pub fn new(deps: SessionDeps, layout: WorkspaceLayout) -> Self {
        Self {
            deps: Some(deps),
            layout: Some(layout),
        }
    }

    /// Decision-only constructor for surfaces that record human gate
    /// decisions without running governed sessions.
    ///
    /// `decide` touches only durable storage; model sessions enter the
    /// pipeline later through queued jobs handled by workers. When
    /// model-provider configuration reaches such a surface it can
    /// switch to [`Self::new`] without any change to decision
    /// semantics.
    pub fn gate_only() -> Self {
        Self {
            deps: None,
            layout: None,
        }
    }

    /// Apply one decision to the run's open plan gate.
    pub async fn decide(
        &self,
        db: &Db,
        org: OrganizationId,
        run: WorkflowRunId,
        input: &ApprovalDecisionInput,
    ) -> Result<ApprovalOutcome> {
        let scope = db.run_scope(run).await?;

        // Replay fast-paths: the durable effects of earlier calls are
        // already visible in run state, so re-derive the outcome from
        // them instead of refusing or double-spending.
        if input.approved
            && scope.state == WorkflowState::Implementing
            && let Some(exec) = db.active_execution_for_run(org, run).await?
        {
            let step = db
                .next_pending_step(org, ExecutionId::from_uuid(exec.id))
                .await?;
            return Ok(ApprovalOutcome::Approved {
                execution_id: ExecutionId::from_uuid(exec.id),
                enqueued_step: step.map(|s| StepId::from_uuid(s.step_id)),
            });
        }
        if !input.approved
            && scope.state == WorkflowState::Planning
            && let Some(gate) = db.get_plan_approval(org, run).await?
            && gate.decision.as_deref() == Some("rejected")
        {
            return Ok(ApprovalOutcome::Rejected {
                approval_id: hephaestus_core::id::HephaestusId(gate.id),
            });
        }

        if scope.state != WorkflowState::AwaitingApproval {
            return Err(Error::Conflict {
                message: format!(
                    "run is {}, not awaiting_approval; no decision can be applied",
                    scope.state.name()
                ),
            });
        }
        let open = db
            .get_plan_approval(org, run)
            .await?
            .ok_or(Error::Conflict {
                message: "no plan approval gate exists for this run".into(),
            })?;
        if open.decision.is_some() {
            return Err(Error::Conflict {
                message: "plan approval gate already decided".into(),
            });
        }
        let plan = db
            .get_current_plan(org, scope.task_id)
            .await?
            .ok_or(Error::Validation {
                field: "plan".into(),
                message: "run awaits approval but has no current plan".into(),
            })?;

        // Durable human decision + typed event in one transaction.
        let approval_id = db
            .decide_plan_approval(
                org,
                run,
                input.approved,
                &input.approver,
                input.reason.as_deref(),
            )
            .await?;

        if input.approved {
            // Bootstrap BEFORE the transition: execution creation is
            // idempotent per run, so a crash here replays cleanly via
            // the state fast-path above.
            let exec = db
                .create_execution_for_run(org, scope.task_id, run, plan.id)
                .await?;
            let pending = db.next_pending_step(org, exec).await?;
            db.transition_run(
                org,
                run,
                WorkflowState::AwaitingApproval,
                TransitionEvent::Approve,
                "approval-service",
            )
            .await?;
            let enqueued_step = match pending {
                Some(step) => {
                    let step_id = StepId::from_uuid(step.step_id);
                    chain_job(
                        db,
                        Queue::Implementation.as_str(),
                        &format!("execute-step:{}:{}", step.step_id, run),
                        run,
                        JobPayload::ExecuteStep {
                            execution_id: exec,
                            step_id,
                            plan_id: plan.id,
                            run_id: run,
                        },
                    )
                    .await?;
                    Some(step_id)
                }
                None => None,
            };
            Ok(ApprovalOutcome::Approved {
                execution_id: exec,
                enqueued_step,
            })
        } else {
            db.transition_run(
                org,
                run,
                WorkflowState::AwaitingApproval,
                TransitionEvent::RejectPlan,
                "approval-service",
            )
            .await?;
            // Fresh key per round so replanning is not swallowed by
            // the idempotent enqueue of the previous round job.
            chain_job(
                db,
                Queue::Planning.as_str(),
                &format!("generate-plan-rev:{}:{}", approval_id, run),
                run,
                JobPayload::GeneratePlan {
                    task_id: scope.task_id,
                    run_id: run,
                },
            )
            .await?;
            Ok(ApprovalOutcome::Rejected {
                approval_id: hephaestus_core::id::HephaestusId(approval_id),
            })
        }
    }
}
