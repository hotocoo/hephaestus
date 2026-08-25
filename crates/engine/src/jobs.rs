//! Versioned job payloads crossing the queue.
//!
//! Payloads are the durable contract between intake and workers. They
//! carry schema versions; workers reject unknown versions loudly
//! instead of guessing, keeping rolling upgrades honest.

use serde::{Deserialize, Serialize};

use hephaestus_core::id::{ExecutionId, PlanId, StepId, TaskId, WorkflowRunId};

/// Current payload schema version.
///
/// v2 adds the execution-phase payloads (`repair_execution`,
/// `run_review`). v1 workers reject v2 envelopes loudly instead of
/// guessing; v2 workers still decode v1 envelopes for rolling
/// upgrades.
pub const JOB_SCHEMA_VERSION: u32 = 2;

/// Every queue name in the system.
///
/// Queues partition work so operators can size and monitor worker
/// pools independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Queue {
    /// Repository snapshot + analysis.
    Analysis,
    /// Requirement extraction and plan generation (model-backed).
    Planning,
    /// Implementation steps (agent/tool execution).
    Implementation,
    /// Deterministic verification layers.
    Verification,
    /// Automated review of implemented changes.
    Review,
    /// Build + artifact production.
    Build,
    /// Deployment and post-deployment verification.
    Deployment,
}

impl Queue {
    /// Canonical queue string used in the jobs table.
    pub fn as_str(self) -> &'static str {
        match self {
            Queue::Analysis => "analysis",
            Queue::Planning => "planning",
            Queue::Implementation => "implementation",
            Queue::Verification => "verification",
            Queue::Review => "review",
            Queue::Build => "build",
            Queue::Deployment => "deployment",
        }
    }

    /// Every queue in the system, in declaration order.
    ///
    /// Lets other layers pin their own vocabulary against the
    /// authoritative enum instead of restating it by hand.
    pub fn iter_all() -> impl Iterator<Item = Queue> {
        [
            Queue::Analysis,
            Queue::Planning,
            Queue::Implementation,
            Queue::Verification,
            Queue::Review,
            Queue::Build,
            Queue::Deployment,
        ]
        .into_iter()
    }
}

/// The unit of work a worker pulls from a queue.
///
/// Every variant carries the workflow run id so progress reporting is
/// uniform, plus whatever identifiers the handler needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum JobPayload {
    /// Clone/update the repository snapshot for this run.
    AnalyzeRepository {
        /// Owning task.
        task_id: TaskId,
        /// Driving run.
        run_id: WorkflowRunId,
    },
    /// Extract structured requirements from task input.
    ExtractRequirements {
        /// Owning task.
        task_id: TaskId,
        /// Driving run.
        run_id: WorkflowRunId,
    },
    /// Generate an implementation plan (requires model provider).
    GeneratePlan {
        /// Owning task.
        task_id: TaskId,
        /// Driving run.
        run_id: WorkflowRunId,
    },
    /// Execute one approved plan step.
    ExecuteStep {
        /// Owning execution attempt.
        execution_id: ExecutionId,
        /// The step to execute.
        step_id: StepId,
        /// Owning plan.
        plan_id: PlanId,
        /// Driving run.
        run_id: WorkflowRunId,
    },
    /// Run the verification suite against current workspace state.
    RunVerification {
        /// Owning execution attempt.
        execution_id: ExecutionId,
        /// Driving run.
        run_id: WorkflowRunId,
    },
    /// Send the workspace back to a governed implementer to repair
    /// failures before verification or review runs again.
    RepairExecution {
        /// Owning execution attempt.
        execution_id: ExecutionId,
        /// Driving run.
        run_id: WorkflowRunId,
        /// What triggered the repair.
        cause: RepairCause,
    },
    /// Run the automated reviewer over the current change set.
    RunReview {
        /// Owning execution attempt.
        execution_id: ExecutionId,
        /// Driving run.
        run_id: WorkflowRunId,
    },
    /// Build artifacts from verified sources.
    BuildArtifact {
        /// Owning task.
        task_id: TaskId,
        /// Driving run.
        run_id: WorkflowRunId,
    },
}

/// Why an execution entered a repair round. The repair session is
/// framed differently depending on who found the problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairCause {
    /// Deterministic verification layers failed.
    Verification,
    /// The automated reviewer requested changes.
    Review,
}

impl JobPayload {
    /// Which queue services this payload.
    pub fn queue(&self) -> Queue {
        match self {
            JobPayload::AnalyzeRepository { .. } | JobPayload::ExtractRequirements { .. } => {
                Queue::Analysis
            }
            JobPayload::GeneratePlan { .. } => Queue::Planning,
            JobPayload::ExecuteStep { .. } | JobPayload::RepairExecution { .. } => {
                Queue::Implementation
            }
            JobPayload::RunVerification { .. } => Queue::Verification,
            JobPayload::RunReview { .. } => Queue::Review,
            JobPayload::BuildArtifact { .. } => Queue::Build,
        }
    }

    /// Serialize into the wire envelope stored in the jobs table.
    pub fn to_envelope(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::json!({
            "schema_version": JOB_SCHEMA_VERSION,
            "payload": self,
        })
        .pipe(Ok)
    }

    /// Decode an envelope, rejecting unknown schema versions.
    pub fn from_envelope(value: &serde_json::Value) -> Result<Self, DecodeError> {
        let version = value
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .ok_or(DecodeError::MissingVersion)?;
        if version != u64::from(JOB_SCHEMA_VERSION) {
            return Err(DecodeError::UnsupportedVersion {
                found: version,
                supported: JOB_SCHEMA_VERSION,
            });
        }
        serde_json::from_value(
            value
                .get("payload")
                .cloned()
                .ok_or(DecodeError::MissingPayload)?,
        )
        .map_err(|_| DecodeError::MalformedPayload)
    }
}

/// Envelope decode failures. Loud by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Envelope lacks a schema version.
    MissingVersion,
    /// Version is newer/older than this worker understands.
    UnsupportedVersion {
        /// Version present in the envelope.
        found: u64,
        /// Version this build speaks.
        supported: u32,
    },
    /// Envelope lacks its payload.
    MissingPayload,
    /// Payload does not deserialize to a known kind.
    MalformedPayload,
}

trait Pipe: Sized {
    fn pipe<F, T>(self, f: F) -> T
    where
        F: FnOnce(Self) -> T,
    {
        f(self)
    }
}
impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn envelope_round_trips() {
        let job = JobPayload::AnalyzeRepository {
            task_id: TaskId::generate(),
            run_id: WorkflowRunId::generate(),
        };
        let env = job.to_envelope().expect("serialize");
        let back = JobPayload::from_envelope(&env).expect("decode");
        assert_eq!(back, job);
    }

    #[test]
    fn future_versions_are_rejected_loudly() {
        let env = serde_json::json!({
            "schema_version": JOB_SCHEMA_VERSION + 99,
            "payload": {},
        });
        let err = JobPayload::from_envelope(&env).expect_err("must fail");
        assert!(matches!(err, DecodeError::UnsupportedVersion { .. }));
    }

    #[test]
    fn missing_fields_rejected() {
        let err = JobPayload::from_envelope(&serde_json::json!({}))
            .expect_err("missing version must fail");
        assert_eq!(err, DecodeError::MissingVersion);

        // Current version but no payload at all.
        let env = serde_json::json!({ "schema_version": JOB_SCHEMA_VERSION });
        let err2 = JobPayload::from_envelope(&env).expect_err("missing payload");
        assert_eq!(err2, DecodeError::MissingPayload);

        // A previous-version envelope is rejected loudly, not parsed.
        let legacy = serde_json::json!({ "schema_version": 1 });
        let err3 = JobPayload::from_envelope(&legacy).expect_err("legacy envelope");
        assert!(matches!(
            err3,
            DecodeError::UnsupportedVersion {
                supported: JOB_SCHEMA_VERSION,
                ..
            }
        ));
    }

    #[test]
    fn queues_route_by_kind() {
        let analysis = JobPayload::AnalyzeRepository {
            task_id: TaskId::generate(),
            run_id: WorkflowRunId::generate(),
        };
        assert_eq!(analysis.queue().as_str(), "analysis");
        let verification = JobPayload::RunVerification {
            execution_id: ExecutionId::generate(),
            run_id: WorkflowRunId::generate(),
        };
        assert_eq!(verification.queue().as_str(), "verification");
    }
}
