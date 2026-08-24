//! Core domain objects.
//!
//! These are the in-memory shapes shared by persistence, API and the
//! engines. Persistence mapping lives in `forge-db`; wire mapping in
//! the API layer. This module owns validation rules that must hold
//! everywhere (e.g. a requirement kind is never silently upgraded).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::id::{
    ExecutionId, OrganizationId, PlanId, ProjectId, RepositoryId, RequirementId, StepId, TaskId,
};

/// Priority of a task. Ordered from most to least urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Drop everything.
    Critical,
    /// Next up.
    High,
    /// Normal queue position.
    Medium,
    /// When there is slack.
    Low,
}

/// Risk classification of a task or change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Trivial, easily reversible change.
    Low,
    /// Ordinary feature work.
    Medium,
    /// Touches critical paths; needs review attention.
    High,
    /// Security/data/deployment-critical; approval required by default.
    Critical,
}

/// Epistemic status of a statement about the task.
///
/// Forge NEVER silently converts an Assumption into a Requirement:
/// assumptions are surfaced to humans and tracked until resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    /// Stated explicitly in task input.
    Explicit,
    /// Derived deterministically from repository facts.
    Inferred,
    /// Guessed; requires human confirmation before it gates anything.
    Assumption,
    /// Known-unknown; blocks planning until resolved.
    Unknown,
}

/// A category of requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementCategory {
    /// Functional behavior.
    Functional,
    /// Non-functional attributes (perf, reliability...).
    NonFunctional,
    /// Security properties.
    Security,
    /// Performance targets.
    Performance,
    /// Backward compatibility constraints.
    Compatibility,
    /// Constraints on how the work may be done.
    Constraint,
}

/// A structured requirement extracted from a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    /// Stable identifier.
    pub id: RequirementId,
    /// Owning task.
    pub task_id: TaskId,
    /// Category bucket.
    pub category: RequirementCategory,
    /// Epistemic status.
    pub kind: RequirementKind,
    /// The statement itself.
    pub statement: String,
    /// Where this came from (task text section, repo fact, ...).
    pub source: String,
    /// Creation time.
    pub created_at: DateTime<Utc>,
}

impl Requirement {
    /// Validate construction invariants.
    ///
    /// Statements must be non-empty after trimming. Assumptions must
    /// carry a non-empty source naming what would confirm them -
    /// otherwise they are downgraded to Unknown, never silently
    /// treated as gating requirements.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: RequirementId,
        task_id: TaskId,
        category: RequirementCategory,
        mut kind: RequirementKind,
        statement: String,
        source: String,
        created_at: DateTime<Utc>,
    ) -> Result<Self> {
        let trimmed = statement.trim();
        if trimmed.is_empty() {
            return Err(Error::Validation {
                field: "statement".into(),
                message: "requirement statement must not be empty".into(),
            });
        }
        if trimmed.len() > 8192 {
            return Err(Error::Validation {
                field: "statement".into(),
                message: "requirement statement exceeds 8 KiB".into(),
            });
        }
        if kind == RequirementKind::Assumption && source.trim().is_empty() {
            // Fail closed: an unexplained guess is recorded as Unknown.
            kind = RequirementKind::Unknown;
        }
        Ok(Self {
            id,
            task_id,
            category,
            kind,
            statement: trimmed.to_string(),
            source: source.trim().to_string(),
            created_at,
        })
    }

    /// Whether this requirement can gate verification decisions.
    ///
    /// Only explicit and inferred requirements gate; assumptions need
    /// human confirmation first.
    pub fn is_gating(&self) -> bool {
        matches!(
            self.kind,
            RequirementKind::Explicit | RequirementKind::Inferred
        )
    }
}

/// Acceptance criterion: a verifiable condition for completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// Human-readable description of the criterion.
    pub description: String,
    /// How it will be checked (verification layer reference).
    pub check: String,
}

/// An engineering task: the top-level unit of requested work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Stable identifier.
    pub id: TaskId,
    /// Tenant scope.
    pub organization_id: OrganizationId,
    /// Owning project.
    pub project_id: ProjectId,
    /// Target repository.
    pub repository_id: RepositoryId,
    /// Short title.
    pub title: String,
    /// Full description (untrusted content; provenance-tagged downstream).
    pub description: String,
    /// Priority.
    pub priority: Priority,
    /// Declared risk level (may be raised by analysis, never lowered silently).
    pub risk: RiskLevel,
    /// Labels for organization/filtering.
    pub labels: Vec<String>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// Validate and construct a new task.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TaskId,
        organization_id: OrganizationId,
        project_id: ProjectId,
        repository_id: RepositoryId,
        title: String,
        description: String,
        priority: Priority,
        risk: RiskLevel,
        labels: Vec<String>,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(Error::Validation {
                field: "title".into(),
                message: "task title must not be empty".into(),
            });
        }
        if title.len() > 512 {
            return Err(Error::Validation {
                field: "title".into(),
                message: "task title exceeds 512 characters".into(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        let mut normalized = Vec::with_capacity(labels.len());
        for label in &labels {
            let l = label.trim().to_lowercase();
            if l.is_empty() || l.len() > 64 || !seen.insert(l.clone()) {
                return Err(Error::Validation {
                    field: "labels".into(),
                    message: format!("invalid or duplicate label: {label:?}"),
                });
            }
            normalized.push(l);
        }
        Ok(Self {
            id,
            organization_id,
            project_id,
            repository_id,
            title,
            description,
            priority,
            risk,
            labels: normalized,
            created_at: now,
            updated_at: now,
        })
    }
}

/// One executable step of a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    /// Stable identifier.
    pub id: StepId,
    /// Owning plan.
    pub plan_id: PlanId,
    /// 1-based order of execution.
    pub position: u32,
    /// What to do. Must name concrete artifacts (files/symbols/tests),
    /// not vague goals.
    pub action: String,
    /// Verification for this step (layer + check reference).
    pub verification: String,
    /// Risks introduced or mitigated by this step.
    pub risks: Vec<String>,
}

/// Rollback / deployment / verification strategy summary of a plan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StrategyNotes {
    /// How to roll back the change safely.
    pub rollback: Option<String>,
    /// How the change reaches environments.
    pub deployment: Option<String>,
    /// Which verification layers must run.
    pub verification: Vec<String>,
}

/// An implementation plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Stable identifier.
    pub id: PlanId,
    /// Owning task.
    pub task_id: TaskId,
    /// Objective restated concretely.
    pub objective: String,
    /// Steps, ordered.
    pub steps: Vec<PlanStep>,
    /// Affected component names (from repository intelligence).
    pub affected_components: Vec<String>,
    /// Affected symbols (fully qualified where possible).
    pub affected_symbols: Vec<String>,
    /// Strategy notes.
    pub strategy: StrategyNotes,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Prompt version that produced this plan (reproducibility).
    pub prompt_version: Option<String>,
}

impl Plan {
    /// Validate plan invariants: non-empty objective, at least one
    /// step, positions contiguous starting at 1, every step names an
    /// action and a verification check.
    pub fn validate(&self) -> Result<()> {
        if self.objective.trim().is_empty() {
            return Err(Error::Validation {
                field: "objective".into(),
                message: "plan objective must not be empty".into(),
            });
        }
        if self.steps.is_empty() {
            return Err(Error::Validation {
                field: "steps".into(),
                message: "plan must contain at least one step".into(),
            });
        }
        for (idx, step) in self.steps.iter().enumerate() {
            if step.position != idx as u32 + 1 {
                return Err(Error::Validation {
                    field: "steps".into(),
                    message: format!(
                        "step positions must be contiguous from 1; expected {}, got {}",
                        idx + 1,
                        step.position
                    ),
                });
            }
            if step.action.trim().is_empty() || step.verification.trim().is_empty() {
                return Err(Error::Validation {
                    field: "steps".into(),
                    message: "every step needs a non-empty action and verification".into(),
                });
            }
        }
        Ok(())
    }
}

/// Lifecycle state of an execution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// Created, not started.
    Pending,
    /// Currently running.
    Running,
    /// Finished successfully.
    Succeeded,
    /// Finished with failure (reason persisted).
    Failed,
    /// Stopped by policy/budget/human.
    Cancelled,
}

/// An execution attempt against a task (one pass of implement+verify).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Execution {
    /// Stable identifier.
    pub id: ExecutionId,
    /// Owning task.
    pub task_id: TaskId,
    /// Attempt number (1-based); retries increment.
    pub attempt: u32,
    /// Current status.
    pub status: ExecutionStatus,
    /// Model provider used, e.g. "openai-compatible" (no secrets).
    pub provider: String,
    /// Model identifier used (no secrets).
    pub model: String,
    /// Prompt pack version used (reproducibility).
    pub prompt_version: String,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Completion timestamp when finished.
    pub finished_at: Option<DateTime<Utc>>,
    /// Failure/stop reason when not successful.
    pub stop_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::StepId;

    fn ts() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn empty_requirement_statement_rejected() {
        let err = Requirement::new(
            RequirementId::generate(),
            TaskId::generate(),
            RequirementCategory::Functional,
            RequirementKind::Explicit,
            "   ".into(),
            "task-input".into(),
            ts(),
        );
        assert!(matches!(err, Err(Error::Validation { .. })));
    }

    #[test]
    fn assumption_without_source_becomes_unknown() {
        let r = Requirement::new(
            RequirementId::generate(),
            TaskId::generate(),
            RequirementCategory::Functional,
            RequirementKind::Assumption,
            "Maybe the API version changed".into(),
            "  ".into(),
            ts(),
        )
        .expect("valid requirement");
        assert_eq!(r.kind, RequirementKind::Unknown);
        assert!(!r.is_gating(), "unknowns must not gate");
    }

    #[test]
    fn gating_requires_explicit_or_inferred() {
        let explicit = Requirement::new(
            RequirementId::generate(),
            TaskId::generate(),
            RequirementCategory::Security,
            RequirementKind::Explicit,
            "Upload endpoint returns 413 over limit".into(),
            "task-input:description".into(),
            ts(),
        )
        .expect("ok");
        assert!(explicit.is_gating());
    }

    #[test]
    fn task_rejects_duplicate_labels_case_insensitively() {
        let t = Task::new(
            TaskId::generate(),
            OrganizationId::generate(),
            ProjectId::generate(),
            RepositoryId::generate(),
            " Fix null deref ".into(),
            "details".into(),
            Priority::High,
            RiskLevel::Medium,
            vec!["Bug".into(), " bug ".into()],
            ts(),
        );
        assert!(
            matches!(t, Err(Error::Validation { ref field, .. }) if field == "labels"),
            "duplicate labels must be rejected"
        );
    }

    #[test]
    fn task_normalizes_labels() {
        let ok = Task::new(
            TaskId::generate(),
            OrganizationId::generate(),
            ProjectId::generate(),
            RepositoryId::generate(),
            "Fix null deref".into(),
            "d".into(),
            Priority::High,
            RiskLevel::Medium,
            vec!["Bug".into(), "Backend".into()],
            ts(),
        )
        .expect("valid task");
        assert_eq!(ok.labels, vec!["bug", "backend"]);
        assert_eq!(ok.title, "Fix null deref");
    }

    #[test]
    fn plan_positions_must_be_contiguous() {
        let plan = Plan {
            id: PlanId::generate(),
            task_id: TaskId::generate(),
            objective: "Fix null dereference in UserService.find".into(),
            steps: vec![
                PlanStep {
                    id: StepId::generate(),
                    plan_id: PlanId::generate(),
                    position: 1,
                    action: "Guard UserService.find against None rows".into(),
                    verification: "unit:user_service_find_none".into(),
                    risks: vec![],
                },
                PlanStep {
                    id: StepId::generate(),
                    plan_id: PlanId::generate(),
                    position: 3,
                    action: "Add regression test".into(),
                    verification: "unit:regression".into(),
                    risks: vec![],
                },
            ],
            affected_components: vec!["services/user".into()],
            affected_symbols: vec!["UserService.find".into()],
            strategy: StrategyNotes::default(),
            created_at: ts(),
            prompt_version: Some("planner-v1".into()),
        };
        assert!(plan.validate().is_err());
    }

    #[test]
    fn valid_plan_passes_validation() {
        let pid = PlanId::generate();
        let plan = Plan {
            id: pid,
            task_id: TaskId::generate(),
            objective: "Fix null dereference in UserService.find".into(),
            steps: vec![
                PlanStep {
                    id: StepId::generate(),
                    plan_id: pid,
                    position: 1,
                    action: "Guard UserService.find against None rows".into(),
                    verification: "unit:user_service_find_none".into(),
                    risks: vec![],
                },
                PlanStep {
                    id: StepId::generate(),
                    plan_id: pid,
                    position: 2,
                    action: "Add regression test find_returns_none_for_missing_user".into(),
                    verification: "unit:regression".into(),
                    risks: vec![],
                },
            ],
            affected_components: vec![],
            affected_symbols: vec![],
            strategy: StrategyNotes::default(),
            created_at: ts(),
            prompt_version: None,
        };
        plan.validate().expect("plan should validate");
    }
}
