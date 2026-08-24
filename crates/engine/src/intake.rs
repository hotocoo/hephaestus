//! Task intake: normalize channel inputs into the internal model.
//!
//! All channels (API, CLI, webhooks, MCP) funnel through this service
//! so validation, idempotency, and run bootstrapping behave
//! identically. Domain validation happens here (pure rules from
//! hephaestus-core); atomic persistence happens in hephaestus-db.

use hephaestus_core::domain::{Priority, RiskLevel, Task};
use hephaestus_core::id::{OrganizationId, ProjectId, RepositoryId};
use hephaestus_core::{Error, Result};
use hephaestus_db::Db;
use hephaestus_db::intake::IntakeCommand;

use crate::jobs::JobPayload;

/// A validated intake request from any channel.
#[derive(Debug, Clone)]
pub struct IntakeRequest {
    /// Tenant scope.
    pub organization_id: OrganizationId,
    /// Project scope.
    pub project_id: ProjectId,
    /// Target repository.
    pub repository_id: RepositoryId,
    /// Task title.
    pub title: String,
    /// Full description; untrusted task input downstream.
    pub description: String,
    /// Priority.
    pub priority: Priority,
    /// Declared risk level.
    pub risk: RiskLevel,
    /// Labels.
    pub labels: Vec<String>,
    /// Optional client idempotency key: retries return the original.
    pub idempotency_key: Option<String>,
}

/// Receipt of an accepted task.
#[derive(Debug, Clone)]
pub struct IntakeReceipt {
    /// The created (or pre-existing) task.
    pub task_id: hephaestus_core::id::TaskId,
    /// Workflow run bootstrapped for the task.
    pub run_id: hephaestus_core::id::WorkflowRunId,
    /// True when an existing task matched the idempotency key.
    pub deduplicated: bool,
}

/// Queue rank derived from declared priority (jobs accept -100..100).
fn priority_rank(p: Priority) -> i16 {
    match p {
        Priority::Critical => 50,
        Priority::High => 25,
        Priority::Medium => 0,
        Priority::Low => -25,
    }
}

fn priority_name(p: Priority) -> &'static str {
    match p {
        Priority::Critical => "critical",
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

fn risk_name(r: RiskLevel) -> &'static str {
    match r {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
        RiskLevel::Critical => "critical",
    }
}

/// Intake service.
#[derive(Debug, Clone)]
pub struct IntakeService {
    db: Db,
}

impl IntakeService {
    /// Bind the service to a database handle.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Validate and accept a task for execution.
    ///
    /// The first job (`AnalyzeRepository`) is enqueued atomically with
    /// task+run creation by the db layer; workers pick it up as soon
    /// as the transaction commits.
    pub async fn submit(&self, req: &IntakeRequest) -> Result<IntakeReceipt> {
        // Pure domain validation. A throwaway ID is fine: persistence
        // assigns its own UUIDv7 inside the transaction.
        Task::new(
            hephaestus_core::id::TaskId::generate(),
            req.organization_id,
            req.project_id,
            req.repository_id,
            req.title.clone(),
            req.description.clone(),
            req.priority,
            req.risk,
            req.labels.clone(),
            chrono::Utc::now(),
        )?;

        // Bootstrap payload targets analysis queue; schema-versioned.
        let bootstrap = JobPayload::AnalyzeRepository {
            task_id: hephaestus_core::id::TaskId::generate(),
            run_id: hephaestus_core::id::WorkflowRunId::generate(),
        };
        let envelope = bootstrap
            .to_envelope()
            .map_err(|e| Error::Storage(Box::new(e)))?;

        let cmd = IntakeCommand {
            organization_id: req.organization_id.as_uuid(),
            project_id: req.project_id.as_uuid(),
            repository_id: req.repository_id.as_uuid(),
            title: req.title.trim().to_string(),
            description: req.description.clone(),
            priority: priority_name(req.priority).to_string(),
            risk: risk_name(req.risk).to_string(),
            labels: req.labels.iter().map(|l| l.trim().to_lowercase()).collect(),
            idempotency_key: req.idempotency_key.clone(),
            first_queue: crate::jobs::Queue::Analysis.as_str().to_string(),
            first_priority: priority_rank(req.priority),
            first_payload: envelope,
        };

        let record = self.db.intake_task(&cmd).await?;
        Ok(IntakeReceipt {
            task_id: hephaestus_core::id::TaskId::from_uuid(record.task_id),
            run_id: hephaestus_core::id::WorkflowRunId::from_uuid(record.run_id),
            deduplicated: record.deduplicated,
        })
    }
}
