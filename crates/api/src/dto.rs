//! Request and response shapes (the public wire contract).
//!
//! Wire types are decoupled from storage rows on purpose: rows change
//! with schema details, this contract changes only deliberately. Domain
//! documents that are already versioned serde types (plans) pass
//! through unchanged rather than being mirrored field by field.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use hephaestus_core::domain::{Priority, RiskLevel};
use hephaestus_db::catalog::{ProjectRow, RepositoryRow};
use hephaestus_db::deployment::DeploymentRow;
use hephaestus_db::events::EventRecord;
use hephaestus_db::planning::ApprovalRow;
use hephaestus_db::tasks::TaskRow;
use hephaestus_db::workflow::WorkflowRunRow;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ApiError;
use crate::auth::Principal;

/// Shared pagination query (`limit` clamped by stores, `offset` non-negative).
#[derive(Debug, Deserialize)]
pub struct Page {
    /// Page size.
    pub limit: Option<i64>,
    /// Page offset.
    pub offset: Option<i64>,
}

impl Page {
    /// Bounded limit value.
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50)
    }

    /// Non-negative offset.
    pub fn offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

/// Submit-task request body.
#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    /// Task title.
    pub title: String,
    /// Full description; untrusted task input downstream.
    pub description: String,
    /// Target project within the caller's organization.
    pub project_id: Uuid,
    /// Target repository within the caller's organization.
    pub repository_id: Uuid,
    /// Priority name; defaults to medium.
    pub priority: Option<String>,
    /// Risk level name; defaults to medium.
    pub risk: Option<String>,
    /// Labels.
    pub labels: Option<Vec<String>>,
}

/// Parse a priority name onto the domain enum.
fn parse_priority(raw: &str) -> Result<Priority, ApiError> {
    match raw.trim().to_lowercase().as_str() {
        "critical" => Ok(Priority::Critical),
        "high" => Ok(Priority::High),
        "medium" | "" => Ok(Priority::Medium),
        "low" => Ok(Priority::Low),
        _ => Err(ApiError(hephaestus_core::Error::Validation {
            field: "priority".into(),
            message: "must be one of critical|high|medium|low".into(),
        })),
    }
}

/// Parse a risk level name onto the domain enum.
fn parse_risk(raw: &str) -> Result<RiskLevel, ApiError> {
    match raw.trim().to_lowercase().as_str() {
        "low" => Ok(RiskLevel::Low),
        "critical" => Ok(RiskLevel::Critical),
        "high" => Ok(RiskLevel::High),
        "medium" | "" => Ok(RiskLevel::Medium),
        _ => Err(ApiError(hephaestus_core::Error::Validation {
            field: "risk".into(),
            message: "must be one of low|medium|high|critical".into(),
        })),
    }
}

impl CreateTaskRequest {
    /// Resolve priority with its default.
    pub fn resolved_priority(&self) -> Result<Priority, ApiError> {
        match self.priority.as_deref() {
            None => Ok(Priority::Medium),
            Some(raw) => parse_priority(raw),
        }
    }

    /// Resolve risk with its default.
    pub fn resolved_risk(&self) -> Result<RiskLevel, ApiError> {
        match self.risk.as_deref() {
            None => Ok(RiskLevel::Medium),
            Some(raw) => parse_risk(raw),
        }
    }
}

/// Receipt of an accepted task.
#[derive(Debug, Serialize)]
pub struct IntakeResponse {
    /// The created (or pre-existing) task.
    pub task_id: Uuid,
    /// Workflow run bootstrapped for the task.
    pub run_id: Uuid,
    /// True when an existing task matched the idempotency key.
    pub deduplicated: bool,
}

/// A task as stored.
#[derive(Debug, Serialize)]
pub struct TaskResponse {
    /// Task id.
    pub id: Uuid,
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Project scope.
    pub project_id: Uuid,
    /// Repository scope.
    pub repository_id: Uuid,
    /// Title.
    pub title: String,
    /// Description.
    pub description: String,
    /// Priority name.
    pub priority: String,
    /// Risk name.
    pub risk: String,
    /// Labels array (JSON).
    pub labels: serde_json::Value,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

impl From<TaskRow> for TaskResponse {
    fn from(row: TaskRow) -> Self {
        Self {
            id: row.id,
            organization_id: row.organization_id,
            project_id: row.project_id,
            repository_id: row.repository_id,
            title: row.title,
            description: row.description,
            priority: row.priority,
            risk: row.risk,
            labels: row.labels,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// A deployment of a built change set to a configured target.
#[derive(Debug, Serialize)]
pub struct DeploymentResponse {
    /// Deployment id.
    pub id: Uuid,
    /// Owning task.
    pub task_id: Uuid,
    /// Driving workflow run.
    pub run_id: Uuid,
    /// Build whose verified change set shipped.
    pub build_id: Uuid,
    /// Configured target name the plan cited.
    pub target: String,
    /// "running", "succeeded" or "failed".
    pub status: String,
    /// Why the deployment failed, when it failed.
    pub failure_reason: Option<String>,
    /// When the deployment row was created.
    pub created_at: DateTime<Utc>,
}

impl From<DeploymentRow> for DeploymentResponse {
    fn from(row: DeploymentRow) -> Self {
        Self {
            id: row.id,
            task_id: row.task_id,
            run_id: row.run_id,
            build_id: row.build_id,
            target: row.target,
            status: row.status,
            failure_reason: row.failure_reason,
            created_at: row.created_at,
        }
    }
}

/// A workflow run as stored.
#[derive(Debug, Serialize)]
pub struct RunResponse {
    /// Run id.
    pub id: Uuid,
    /// Owning task.
    pub task_id: Uuid,
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Current state name.
    pub state: String,
    /// Attempt counter.
    pub attempt: i32,
    /// Correlation id for tracing.
    pub correlation_id: Uuid,
    /// Current lease owner if any.
    pub lease_owner: Option<String>,
    /// Lease expiry if leased.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// Last transition time.
    pub last_transition_at: DateTime<Utc>,
}

impl From<WorkflowRunRow> for RunResponse {
    fn from(row: WorkflowRunRow) -> Self {
        Self {
            id: row.id,
            task_id: row.task_id,
            organization_id: row.organization_id,
            state: row.state,
            attempt: row.attempt,
            correlation_id: row.correlation_id,
            lease_owner: row.lease_owner,
            lease_expires_at: row.lease_expires_at,
            last_transition_at: row.last_transition_at,
        }
    }
}

/// A stored event record; the provenance-tagged payload renders as data.
#[derive(Debug, Serialize)]
pub struct EventResponse {
    /// Event id.
    pub id: Uuid,
    /// Aggregate kind name.
    pub aggregate: String,
    /// Aggregate instance id.
    pub aggregate_id: Uuid,
    /// Provenance classification.
    pub provenance: String,
    /// Payload JSON.
    pub payload: serde_json::Value,
    /// Occurrence time.
    pub occurred_at: DateTime<Utc>,
}

impl From<EventRecord> for EventResponse {
    fn from(rec: EventRecord) -> Self {
        Self {
            id: rec.id,
            aggregate: rec.aggregate,
            aggregate_id: rec.aggregate_id,
            provenance: rec.provenance,
            payload: rec.payload,
            occurred_at: rec.occurred_at,
        }
    }
}

/// An approval gate as stored.
#[derive(Debug, Serialize)]
pub struct GateResponse {
    /// Gate row id.
    pub approval_id: Uuid,
    /// Which gate ('plan' or 'merge').
    pub gate: String,
    /// Role required to decide.
    pub required_role: String,
    /// Decision when made.
    pub decision: Option<String>,
}

impl From<ApprovalRow> for GateResponse {
    fn from(row: ApprovalRow) -> Self {
        Self {
            approval_id: row.id,
            gate: row.gate,
            required_role: row.required_role,
            decision: row.decision,
        }
    }
}

/// Plan-approval decision request body.
#[derive(Debug, Deserialize)]
pub struct ApprovalDecisionRequest {
    /// Approve or reject the current plan.
    pub approved: bool,
    /// Free-form rationale, persisted with the decision.
    pub reason: Option<String>,
}

/// Outcome of applying a plan-approval decision.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApprovalDecisionResponse {
    /// Plan approved: an active execution covers it.
    Approved {
        /// The active execution for this run.
        execution_id: Uuid,
        /// Step enqueued for implementation, when one was pending.
        enqueued_step: Option<Uuid>,
    },
    /// Plan rejected: the run returned to planning for revision.
    Rejected {
        /// The gate row carrying the decision.
        approval_id: Uuid,
    },
}

/// Merge decision request body. The merger is the authenticated
/// principal; it is never accepted from the body.
#[derive(Debug, Deserialize)]
pub struct MergeDecisionRequest {
    /// Free-form rationale, persisted with the decision.
    pub reason: Option<String>,
    /// External reference for the merge (commit sha, PR number).
    pub external_ref: Option<String>,
}

/// Outcome of applying a merge decision.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MergeDecisionResponse {
    /// Decision recorded and build chained.
    Merged,
    /// Decision was already applied (replayed request).
    AlreadyMerged,
}

/// A project as stored.
#[derive(Debug, Serialize)]
pub struct ProjectResponse {
    /// Project id.
    pub id: Uuid,
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Display name.
    pub name: String,
    /// URL-safe slug.
    pub slug: String,
}

impl From<ProjectRow> for ProjectResponse {
    fn from(row: ProjectRow) -> Self {
        Self {
            id: row.id,
            organization_id: row.organization_id,
            name: row.name,
            slug: row.slug,
        }
    }
}

/// A repository as stored.
#[derive(Debug, Serialize)]
pub struct RepositoryResponse {
    /// Repository id.
    pub id: Uuid,
    /// Tenant scope.
    pub organization_id: Uuid,
    /// Owning project.
    pub project_id: Uuid,
    /// Remote URL.
    pub remote_url: String,
    /// Branch intake snapshots default to.
    pub default_branch: String,
    /// Display name.
    pub display_name: String,
}

impl From<RepositoryRow> for RepositoryResponse {
    fn from(row: RepositoryRow) -> Self {
        Self {
            id: row.id,
            organization_id: row.organization_id,
            project_id: row.project_id,
            remote_url: row.remote_url,
            default_branch: row.default_branch,
            display_name: row.display_name,
        }
    }
}

/// Extractor for the middleware-resolved principal.
///
/// Handlers take `Principal` directly; a missing principal means
/// the auth middleware did not run for this route, which surfaces as an
/// authentication failure rather than being trusted.
#[derive(Debug, Clone)]
pub struct Authed(pub Principal);

impl<S> FromRequestParts<S> for Authed
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .cloned()
            .map(Authed)
            .ok_or(ApiError(hephaestus_core::Error::Unauthenticated))
    }
}
