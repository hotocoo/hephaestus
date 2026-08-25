//! Planning pipeline: requirement extraction and plan generation.
//!
//! Both stages run through the governed agent runtime ([hephaestus_agent])
//! with the read-only Planner role: model output selects tools or ends
//! the run, every call is capability-checked and audited, and the
//! session's final answer must be EXACTLY one JSON document parsed
//! deterministically below. Nothing the model says widens permissions;
//! anything malformed fails into bounded queue retries rather than
//! being guessed at.
//!
//! Extraction persists requirements atomically and chains plan
//! generation; planning persists the plan, opens the human approval
//! gate, and advances the run to `awaiting_approval`.

use chrono::Utc;
use serde::Deserialize;

use hephaestus_agent::role::AgentRole;
use hephaestus_agent::session::UntrustedFact;
use hephaestus_core::domain::{
    Plan, PlanStep, Requirement, RequirementCategory, RequirementKind, StrategyNotes,
};
use hephaestus_core::id::{PlanId, StepId, TaskId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_core::{Error, Result};
use hephaestus_db::Db;

use crate::analysis::{StageError, WorkspaceLayout, chain_job};
use crate::governed::{SessionDeps, parse_strict, run_role_session};
use crate::jobs::{JobPayload, Queue};
use crate::worker::{HandlerOutcome, JobHandler};

/// Version recorded on plans produced by these prompts
/// (reproducibility anchor when prompts change).
pub const PLAN_PROMPT_VERSION: &str = "planner-v1";

/// Upper bound on requirements accepted from one extraction run.
pub const MAX_REQUIREMENTS: usize = 64;

/// Upper bound on steps accepted in one generated plan.
pub const MAX_STEPS: usize = 32;

// ---------- wire documents (model-facing contract) ----------

/// One requirement as proposed by the model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RequirementDraft {
    /// Category bucket.
    pub category: DraftCategory,
    /// Epistemic status.
    pub kind: DraftKind,
    /// The statement.
    pub statement: String,
    /// Where it came from, or what would confirm an assumption.
    #[serde(default)]
    pub source: String,
}

/// Category strings the planner may emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftCategory {
    /// Functional behavior.
    Functional,
    /// Non-functional attributes.
    NonFunctional,
    /// Security properties.
    Security,
    /// Performance targets.
    Performance,
    /// Compatibility constraints.
    Compatibility,
    /// Constraints on approach.
    Constraint,
}

/// Epistemic-status strings the planner may emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    /// Stated explicitly in task input.
    Explicit,
    /// Derived from repository facts.
    Inferred,
    /// Guessed; needs human confirmation.
    Assumption,
    /// Known-unknown.
    Unknown,
}

/// Extraction-stage final-answer document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RequirementsDoc {
    /// The extracted requirements.
    pub requirements: Vec<RequirementDraft>,
}

/// One plan step as proposed by the model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StepDraft {
    /// Concrete action naming artifacts.
    pub action: String,
    /// Verification reference (layer:check).
    pub verification: String,
    /// Risks introduced or mitigated.
    #[serde(default)]
    pub risks: Vec<String>,
}

/// Planning-stage final-answer document.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PlanDocument {
    /// Objective restated concretely.
    pub objective: String,
    /// Ordered steps (positions assigned server-side).
    pub steps: Vec<StepDraft>,
    /// Affected component names.
    #[serde(default)]
    pub affected_components: Vec<String>,
    /// Affected symbols.
    #[serde(default)]
    pub affected_symbols: Vec<String>,
    /// Rollback / deployment / verification notes.
    #[serde(default)]
    pub strategy: StrategyNotes,
}

fn category_of(c: DraftCategory) -> RequirementCategory {
    match c {
        DraftCategory::Functional => RequirementCategory::Functional,
        DraftCategory::NonFunctional => RequirementCategory::NonFunctional,
        DraftCategory::Security => RequirementCategory::Security,
        DraftCategory::Performance => RequirementCategory::Performance,
        DraftCategory::Compatibility => RequirementCategory::Compatibility,
        DraftCategory::Constraint => RequirementCategory::Constraint,
    }
}

fn kind_of(k: DraftKind) -> RequirementKind {
    match k {
        DraftKind::Explicit => RequirementKind::Explicit,
        DraftKind::Inferred => RequirementKind::Inferred,
        DraftKind::Assumption => RequirementKind::Assumption,
        DraftKind::Unknown => RequirementKind::Unknown,
    }
}

fn kind_label(k: RequirementKind) -> &'static str {
    match k {
        RequirementKind::Explicit => "explicit",
        RequirementKind::Inferred => "inferred",
        RequirementKind::Assumption => "assumption",
        RequirementKind::Unknown => "unknown",
    }
}

/// Map drafts into validated domain requirements.
///
/// [`Requirement::new`] enforces the epistemic rules (empty assumption
/// sources downgrade to Unknown); the count cap fails loudly instead of
/// truncating silently.
pub fn map_requirements(task_id: TaskId, doc: RequirementsDoc) -> Result<Vec<Requirement>> {
    if doc.requirements.len() > MAX_REQUIREMENTS {
        return Err(Error::Validation {
            field: "requirements".into(),
            message: format!(
                "{} requirements exceeds the maximum of {MAX_REQUIREMENTS}",
                doc.requirements.len()
            ),
        });
    }
    let now = Utc::now();
    doc.requirements
        .into_iter()
        .map(|d| {
            Requirement::new(
                hephaestus_core::id::RequirementId::generate(),
                task_id,
                category_of(d.category),
                kind_of(d.kind),
                d.statement,
                d.source,
                now,
            )
        })
        .collect()
}

/// Build a domain plan from the document: server-side identity and
/// contiguous positions, then full invariant validation.
pub fn build_plan(task_id: TaskId, doc: PlanDocument) -> Result<Plan> {
    if doc.objective.trim().len() > 8192 {
        return Err(Error::Validation {
            field: "objective".into(),
            message: "plan objective exceeds 8 KiB".into(),
        });
    }
    if doc.steps.len() > MAX_STEPS {
        return Err(Error::Validation {
            field: "steps".into(),
            message: format!(
                "{} steps exceeds the maximum of {MAX_STEPS}",
                doc.steps.len()
            ),
        });
    }
    let plan_id = PlanId::generate();
    let steps = doc
        .steps
        .into_iter()
        .enumerate()
        .map(|(idx, s)| PlanStep {
            id: StepId::generate(),
            plan_id,
            position: idx as u32 + 1,
            action: s.action,
            verification: s.verification,
            risks: s.risks,
        })
        .collect();
    let plan = Plan {
        id: plan_id,
        task_id,
        objective: doc.objective.trim().to_string(),
        steps,
        affected_components: doc.affected_components,
        affected_symbols: doc.affected_symbols,
        strategy: doc.strategy,
        created_at: Utc::now(),
        prompt_version: Some(PLAN_PROMPT_VERSION.into()),
    };
    plan.validate()?;
    Ok(plan)
}

// ---------- governed session plumbing (shared in crate::governed) ----------

fn extraction_objective() -> String {
    [
        "Extract the engineering requirements stated by the task context.",
        "You may read repository files with fs.read to ground your analysis.",
        "When finished, your final answer MUST be exactly one JSON object of shape:",
        "{\"requirements\":[{\"category\":\"functional|non_functional|security|performance|compatibility|constraint\",",
        "\"kind\":\"explicit|inferred|assumption|unknown\",\"statement\":\"...\",\"source\":\"origin\"}]}",
        "At most 64 requirements. Statements must be concrete and testable.",
        "Use kind=assumption only together with a source naming what would confirm it;",
        "use kind=unknown for open questions that block planning.",
    ]
    .join("\n")
}

fn planning_objective() -> String {
    [
        "Draft an implementation plan for the requirements in the task context.",
        "You may read repository files with fs.read to ground each step.",
        "When finished, your final answer MUST be exactly one JSON object of shape:",
        "{\"objective\":\"...\",\"steps\":[{\"action\":\"concrete change naming files/symbols/tests\",",
        "\"verification\":\"layer:check reference\",\"risks\":[\"...\"]}],",
        "\"affected_components\":[\"...\"],\"affected_symbols\":[\"...\"],",
        "\"strategy\":{\"rollback\":\"...|null\",\"deployment\":\"...|null\",\"verification\":[\"layer\"]}}",
        "At most 32 steps, ordered by execution. Every step needs an action AND a verification.",
    ]
    .join("\n")
}

// ---------- handlers ----------

/// Handler for [`JobPayload::ExtractRequirements`].
pub struct ExtractionHandler {
    deps: SessionDeps,
    layout: WorkspaceLayout,
}

impl ExtractionHandler {
    /// Bind dependencies and workspace layout.
    pub fn new(deps: SessionDeps, layout: WorkspaceLayout) -> Self {
        Self { deps, layout }
    }
}

impl JobHandler for ExtractionHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::ExtractRequirements { task_id, run_id } = payload else {
                tracing::error!(?payload, "extraction handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match extract(&db, &self.deps, &self.layout, task_id, run_id).await {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(task = %task_id, run = %run_id, error = %e, "extraction retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(task = %task_id, run = %run_id, error = %e, "extraction permanent failure");
                    HandlerOutcome::FailedPermanent
                }
            }
        })
    }
}

async fn extract(
    db: &Db,
    deps: &SessionDeps,
    layout: &WorkspaceLayout,
    task_id: TaskId,
    run_id: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    let scope = db.run_scope(run_id).await.map_err(StageError::Permanent)?;
    advance(
        db,
        &scope,
        run_id,
        WorkflowState::Created,
        TransitionEvent::StartAnalysis,
        "extraction-handler",
    )
    .await?;

    let task = db
        .get_task(scope.organization_id, task_id)
        .await
        .map_err(StageError::Permanent)?;
    let facts = [
        UntrustedFact::new("task-title", task.title.clone()),
        UntrustedFact::new("task-description", task.description.clone()),
    ];
    let workspace = layout.run_repo_dir(run_id);
    let final_text = run_role_session(
        deps,
        AgentRole::Planner,
        workspace,
        &extraction_objective(),
        &facts,
    )
    .await?;

    let doc: RequirementsDoc = parse_strict(&final_text).map_err(StageError::Retryable)?;
    let requirements = map_requirements(task_id, doc).map_err(StageError::Retryable)?;
    db.replace_requirements(scope.organization_id, task_id, run_id, &requirements)
        .await
        .map_err(StageError::Retryable)?;

    chain_job(
        db,
        Queue::Planning.as_str(),
        "generate-plan",
        run_id,
        JobPayload::GeneratePlan { task_id, run_id },
    )
    .await
    .map_err(StageError::Retryable)?;
    Ok(())
}

/// Handler for [`JobPayload::GeneratePlan`].
pub struct PlanningHandler {
    deps: SessionDeps,
    layout: WorkspaceLayout,
}

impl PlanningHandler {
    /// Bind dependencies and workspace layout.
    pub fn new(deps: SessionDeps, layout: WorkspaceLayout) -> Self {
        Self { deps, layout }
    }
}

impl JobHandler for PlanningHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::GeneratePlan { task_id, run_id } = payload else {
                tracing::error!(?payload, "planning handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match plan_stage(&db, &self.deps, &self.layout, task_id, run_id).await {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(task = %task_id, run = %run_id, error = %e, "planning retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(task = %task_id, run = %run_id, error = %e, "planning permanent failure");
                    HandlerOutcome::FailedPermanent
                }
            }
        })
    }
}

async fn plan_stage(
    db: &Db,
    deps: &SessionDeps,
    layout: &WorkspaceLayout,
    task_id: TaskId,
    run_id: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    let scope = db.run_scope(run_id).await.map_err(StageError::Permanent)?;

    // Redelivery after full success: the gate is open and the plan
    // exists, so the correct outcome is completion, not a duplicate.
    if scope.state == WorkflowState::AwaitingApproval {
        let existing = db
            .get_current_plan(scope.organization_id, task_id)
            .await
            .map_err(StageError::Permanent)?;
        return if existing.is_some() {
            Ok(())
        } else {
            Err(StageError::Permanent(Error::Validation {
                field: "state".into(),
                message: "run awaits approval but no current plan exists".into(),
            }))
        };
    }

    advance(
        db,
        &scope,
        run_id,
        WorkflowState::Analyzing,
        TransitionEvent::StartPlanning,
        "planning-handler",
    )
    .await?;

    let requirements = db
        .list_requirements(scope.organization_id, task_id)
        .await
        .map_err(StageError::Permanent)?;
    if requirements.is_empty() {
        return Err(StageError::Permanent(Error::Validation {
            field: "requirements".into(),
            message: "plan generation requires extracted requirements".into(),
        }));
    }

    let facts: Vec<UntrustedFact> = requirements
        .iter()
        .enumerate()
        .map(|(i, r)| {
            UntrustedFact::new(
                format!("requirement-{}", i + 1),
                format!("[{}] {}", kind_label(r.kind), r.statement),
            )
        })
        .collect();
    let workspace = layout.run_repo_dir(run_id);
    let final_text = run_role_session(
        deps,
        AgentRole::Planner,
        workspace,
        &planning_objective(),
        &facts,
    )
    .await?;

    let doc: PlanDocument = parse_strict(&final_text).map_err(StageError::Retryable)?;
    let plan = build_plan(task_id, doc).map_err(StageError::Retryable)?;
    db.create_plan(scope.organization_id, task_id, run_id, &plan)
        .await
        .map_err(StageError::Retryable)?;
    db.open_plan_approval(scope.organization_id, task_id, run_id)
        .await
        .map_err(StageError::Retryable)?;

    let reloaded = db.run_scope(run_id).await.map_err(StageError::Permanent)?;
    advance(
        db,
        &reloaded,
        run_id,
        WorkflowState::Planning,
        TransitionEvent::SubmitForApproval,
        "planning-handler",
    )
    .await?;
    Ok(())
}

/// Apply one legal transition unless another worker already did.
async fn advance(
    db: &Db,
    scope: &hephaestus_db::workflow::RunScope,
    run_id: WorkflowRunId,
    expected: WorkflowState,
    trigger: TransitionEvent,
    actor: &str,
) -> std::result::Result<(), StageError> {
    if scope.state != expected {
        return Ok(());
    }
    if let Err(e) = db
        .transition_run(scope.organization_id, run_id, expected, trigger, actor)
        .await
    {
        // Lost race against a concurrent redelivery is benign.
        if !matches!(e, Error::Conflict { .. }) {
            return Err(StageError::Retryable(e));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hephaestus_core::id::TaskId;

    fn req_doc(v: serde_json::Value) -> RequirementsDoc {
        serde_json::from_value(v).expect("valid requirements document")
    }

    #[test]
    fn requirements_document_parses_and_maps() {
        let doc = req_doc(serde_json::json!({
            "requirements": [
                {"category": "functional", "kind": "explicit",
                 "statement": "Upload returns 413 over the size limit.", "source": "task-description"},
                {"category": "security", "kind": "inferred",
                 "statement": "Rejected uploads must not persist partial data.", "source": "repo:upload.rs"},
            ]
        }));
        let task = TaskId::generate();
        let reqs = map_requirements(task, doc).expect("map");
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].category, RequirementCategory::Functional);
        assert_eq!(reqs[0].kind, RequirementKind::Explicit);
        assert!(reqs[0].is_gating());
        // Statements are trimmed by the domain constructor.
        assert_eq!(reqs[0].statement, "Upload returns 413 over the size limit.");
    }

    #[test]
    fn assumption_without_source_downgrades_to_unknown() {
        let doc = req_doc(serde_json::json!({
            "requirements": [
                {"category": "compatibility", "kind": "assumption",
                 "statement": "Maybe the API version changed.", "source": ""}
            ]
        }));
        let reqs = map_requirements(TaskId::generate(), doc).expect("map");
        assert_eq!(reqs[0].kind, RequirementKind::Unknown);
        assert!(!reqs[0].is_gating(), "downgraded unknowns must not gate");
    }

    #[test]
    fn requirement_cap_fails_loudly() {
        let entries: Vec<serde_json::Value> = (0..=MAX_REQUIREMENTS)
            .map(|i| {
                serde_json::json!({"category": "functional", "kind": "explicit",
                    "statement": format!("requirement number {i}"), "source": "test"})
            })
            .collect();
        let doc = req_doc(serde_json::json!({ "requirements": entries }));
        let err = map_requirements(TaskId::generate(), doc)
            .expect_err("over-cap document must be rejected");
        assert!(err.to_string().contains("maximum"));
    }

    #[test]
    fn garbage_final_text_is_rejected_not_guessed_at() {
        for bad in [
            "Sure! Here is my plan.",
            "```json\n{\"requirements\":[]}\n```",
            "",
            "{\"requirements\":[{\"category\": \"platform\", \"kind\": \"explicit\",\n                 \"statement\": \"x\", \"source\": \"y\"}]}",
        ] {
            let res: Result<RequirementsDoc> = parse_strict(bad);
            assert!(res.is_err(), "must reject: {bad:?}");
        }
    }

    fn plan_doc(steps: serde_json::Value) -> PlanDocument {
        serde_json::from_value(serde_json::json!({
            "objective": "Guard UserService.find against missing rows.",
            "steps": steps,
            "affected_components": ["services/user"],
            "strategy": {"rollback": "revert commit", "verification": ["unit"]}
        }))
        .expect("valid plan document")
    }

    #[test]
    fn plan_positions_assigned_contiguously_and_validated() {
        let task = TaskId::generate();
        let doc = plan_doc(serde_json::json!([
            {"action": "Add None guard in UserService.find", "verification": "unit:user_service_find_none", "risks": ["behavior change"]},
            {"action": "Add regression test find_returns_none_for_missing_user", "verification": "unit:regression", "risks": []}
        ]));
        let plan = build_plan(task, doc).expect("plan builds");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].position, 1);
        assert_eq!(plan.steps[1].position, 2);
        assert_ne!(plan.steps[0].id, plan.steps[1].id, "step ids unique");
        assert!(plan.steps.iter().all(|s| s.plan_id == plan.id));
        assert_eq!(plan.prompt_version.as_deref(), Some(PLAN_PROMPT_VERSION));
        assert_eq!(plan.strategy.rollback.as_deref(), Some("revert commit"));
        plan.validate().expect("domain invariants hold");
    }

    #[test]
    fn step_cap_fails_loudly() {
        let steps: Vec<serde_json::Value> = (0..=MAX_STEPS)
            .map(|i| {
                serde_json::json!({"action": format!("step {i}"), "verification": "unit:x", "risks": []})
            })
            .collect();
        let err = build_plan(
            TaskId::generate(),
            plan_doc(serde_json::Value::Array(steps)),
        )
        .expect_err("over-cap plan must be rejected");
        assert!(err.to_string().contains("maximum"));
    }

    #[test]
    fn empty_plan_rejected_by_domain_validation() {
        let err = build_plan(TaskId::generate(), plan_doc(serde_json::json!([])))
            .expect_err("empty plan must fail validation");
        assert!(matches!(err, Error::Validation { field, .. } if field == "steps"));
    }

    #[test]
    fn oversized_objective_rejected_before_sql_sees_it() {
        let mut doc = plan_doc(serde_json::json!([
            {"action": "a", "verification": "v", "risks": []}
        ]));
        doc.objective = "x".repeat(8_193);
        let err =
            build_plan(TaskId::generate(), doc).expect_err("oversized objective must be rejected");
        assert!(err.to_string().contains("8 KiB"));
    }

    #[test]
    fn workspace_layout_paths_are_run_scoped() {
        let layout = WorkspaceLayout::new("/data");
        let run = WorkflowRunId::generate();
        let dir = layout.run_repo_dir(run);
        assert!(dir.starts_with("/data/runs"));
        assert!(dir.ends_with("repo"));
        assert!(dir.to_string_lossy().contains(&run.to_string()));
    }
}
