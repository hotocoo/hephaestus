//! Route table and handlers.
//!
//! Handlers orchestrate nothing: they authenticate through the shared
//! middleware, scope every store/service call by the caller's
//! organization, and translate between wire types and service inputs.
//! All durable effects flow through engine services so their
//! idempotency and audit guarantees hold verbatim over HTTP.

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::routing::post;
use axum::{Json, Router};
use hephaestus_core::Error;
use hephaestus_core::domain::Plan;
use hephaestus_core::event::AggregateKind;
use hephaestus_core::id::{OrganizationId, ProjectId, RepositoryId, TaskId, WorkflowRunId};
use hephaestus_engine::approval::ApprovalDecisionInput;
use hephaestus_engine::delivery::MergeDecisionInput;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::auth_middleware;
use crate::dto::{
    ApprovalDecisionRequest, ApprovalDecisionResponse, Authed, CreateTaskRequest, EventResponse,
    GateResponse, IntakeResponse, MergeDecisionRequest, MergeDecisionResponse, Page,
    ProjectResponse, RepositoryResponse, RunResponse, TaskResponse,
};
use crate::state::AppState;
use crate::{ApiError, ApiResult};

/// Build the application router: public probes plus the authenticated
/// v1 surface under `/api`.
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/projects", get(list_projects))
        .route("/repositories", get(list_repositories))
        .route("/tasks", post(create_task).get(list_tasks))
        .route("/tasks/{task_id}", get(get_task))
        .route("/tasks/{task_id}/plan", get(get_task_plan))
        .route("/tasks/{task_id}/run", get(get_task_run))
        .route("/runs/{run_id}", get(get_run))
        .route("/runs/{run_id}/events", get(list_run_events))
        .route(
            "/runs/{run_id}/approval",
            get(get_approval_gate).post(decide_approval),
        )
        .route("/runs/{run_id}/merge-gate", get(get_merge_gate))
        .route("/runs/{run_id}/merge", post(decide_merge))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

    // The machine-readable contract is public but lives under the
    // versioned prefix, so it nests beside (not inside) the
    // authenticated surface and carries no auth layer of its own.
    let contract = Router::new()
        .route("/openapi.json", get(crate::openapi::serve_document))
        .with_state(state.clone());

    let probes = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state.clone());

    probes
        .nest("/api/v1", contract.merge(protected))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(state.max_body_bytes))
}

/// Liveness: the process is up. No auth, no dependencies.
async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Readiness: dependencies are reachable. No auth; load balancers and
/// orchestrators need an honest signal, not a tenant-scoped one.
async fn readyz(State(state): State<AppState>) -> Response {
    match state.db.ping().await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ready" })),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(error = %err, "readiness probe failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "code": "NOT_READY", "message": "dependencies unreachable" })),
            )
                .into_response()
        }
    }
}

/// Unknown routes render in the shared error shape.
async fn not_found() -> ApiError {
    ApiError(Error::NotFound { entity: "route" })
}

/// Parse a path segment into a typed identifier.
fn id_of(raw: &str, field: &'static str) -> ApiResult<Uuid> {
    Uuid::parse_str(raw.trim()).map_err(|_| {
        ApiError(Error::Validation {
            field: field.into(),
            message: "must be a UUID".into(),
        })
    })
}

#[derive(Debug, Deserialize)]
struct ReposQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    project_id: Option<Uuid>,
}

async fn list_projects(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Query(page): Query<Page>,
) -> ApiResult<Json<Vec<ProjectResponse>>> {
    let rows = state
        .db
        .list_projects(principal.organization_id, page.limit(), page.offset())
        .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

async fn list_repositories(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Query(query): Query<ReposQuery>,
) -> ApiResult<Json<Vec<RepositoryResponse>>> {
    let project = query.project_id.map(ProjectId::from_uuid);
    let rows = state
        .db
        .list_repositories(
            principal.organization_id,
            project,
            query.limit.unwrap_or(50),
            query.offset.unwrap_or(0).max(0),
        )
        .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

async fn create_task(
    State(state): State<AppState>,
    Authed(principal): Authed,
    headers: HeaderMap,
    Json(req): Json<CreateTaskRequest>,
) -> ApiResult<(StatusCode, Json<IntakeResponse>)> {
    // Client idempotency key rides the conventional header; empty
    // values mean absent.
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Resolve enum fields while the request is still whole; moving
    // title/description below consumes it.
    let priority = req.resolved_priority()?;
    let risk = req.resolved_risk()?;
    let intake_req = hephaestus_engine::intake::IntakeRequest {
        organization_id: principal.organization_id,
        project_id: ProjectId::from_uuid(req.project_id),
        repository_id: RepositoryId::from_uuid(req.repository_id),
        title: req.title,
        description: req.description,
        priority,
        risk,
        labels: req.labels.unwrap_or_default(),
        idempotency_key,
    };
    let receipt = state.intake.submit(&intake_req).await?;
    Ok((
        StatusCode::CREATED,
        Json(IntakeResponse {
            task_id: receipt.task_id.as_uuid(),
            run_id: receipt.run_id.as_uuid(),
            deduplicated: receipt.deduplicated,
        }),
    ))
}

async fn list_tasks(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Query(page): Query<Page>,
) -> ApiResult<Json<Vec<TaskResponse>>> {
    let rows = state
        .db
        .list_tasks(principal.organization_id, page.limit(), page.offset())
        .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

async fn get_task(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(task_id): Path<String>,
) -> ApiResult<Json<TaskResponse>> {
    let row = state
        .db
        .get_task(
            principal.organization_id,
            TaskId::from_uuid(id_of(&task_id, "task_id")?),
        )
        .await?;
    Ok(Json(row.into()))
}

/// The current plan for a task; domain serde type rendered directly.
async fn get_task_plan(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(task_id): Path<String>,
) -> ApiResult<Json<Plan>> {
    let org: OrganizationId = principal.organization_id;
    let plan = state
        .db
        .get_current_plan(org, TaskId::from_uuid(id_of(&task_id, "task_id")?))
        .await?
        .ok_or(Error::NotFound { entity: "plan" })?;
    Ok(Json(plan))
}

/// The current workflow run of a task; the dashboard's path from a
/// task page to its live state without knowing the run id.
async fn get_task_run(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(task_id): Path<String>,
) -> ApiResult<Json<RunResponse>> {
    let row = state
        .db
        .get_run_for_task(
            principal.organization_id,
            TaskId::from_uuid(id_of(&task_id, "task_id")?),
        )
        .await?;
    Ok(Json(row.into()))
}

async fn get_run(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(run_id): Path<String>,
) -> ApiResult<Json<RunResponse>> {
    let row = state
        .db
        .get_run(
            principal.organization_id,
            WorkflowRunId::from_uuid(id_of(&run_id, "run_id")?),
        )
        .await?;
    Ok(Json(row.into()))
}

async fn list_run_events(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(run_id): Path<String>,
    Query(page): Query<Page>,
) -> ApiResult<Json<Vec<EventResponse>>> {
    let rows = state
        .db
        .list_events(
            principal.organization_id,
            AggregateKind::WorkflowRun,
            hephaestus_core::id::HephaestusId(id_of(&run_id, "run_id")?),
            page.limit(),
        )
        .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// Gate lookup that renders 404 when no gate exists for the run.
async fn gate_or_404(
    state: &AppState,
    org: OrganizationId,
    run: WorkflowRunId,
    which: &'static str,
) -> ApiResult<GateResponse> {
    let row = match which {
        "plan" => state.db.get_plan_approval(org, run).await?,
        _ => state.db.get_merge_gate(org, run).await?,
    };
    row.map(Into::into)
        .ok_or(ApiError(Error::NotFound { entity: "gate" }))
}

async fn get_approval_gate(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(run_id): Path<String>,
) -> ApiResult<Json<GateResponse>> {
    let run = WorkflowRunId::from_uuid(id_of(&run_id, "run_id")?);
    Ok(Json(
        gate_or_404(&state, principal.organization_id, run, "plan").await?,
    ))
}

async fn decide_approval(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(run_id): Path<String>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> ApiResult<Json<ApprovalDecisionResponse>> {
    let run = WorkflowRunId::from_uuid(id_of(&run_id, "run_id")?);
    let outcome = state
        .approvals
        .decide(
            &state.db,
            principal.organization_id,
            run,
            &ApprovalDecisionInput {
                approver: principal.principal,
                approved: req.approved,
                reason: req.reason,
            },
        )
        .await?;
    let rendered = match outcome {
        hephaestus_engine::approval::ApprovalOutcome::Approved {
            execution_id,
            enqueued_step,
        } => ApprovalDecisionResponse::Approved {
            execution_id: execution_id.as_uuid(),
            enqueued_step: enqueued_step.map(|s| s.as_uuid()),
        },
        hephaestus_engine::approval::ApprovalOutcome::Rejected { approval_id } => {
            ApprovalDecisionResponse::Rejected {
                approval_id: approval_id.0,
            }
        }
    };
    Ok(Json(rendered))
}

async fn get_merge_gate(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(run_id): Path<String>,
) -> ApiResult<Json<GateResponse>> {
    let run = WorkflowRunId::from_uuid(id_of(&run_id, "run_id")?);
    Ok(Json(
        gate_or_404(&state, principal.organization_id, run, "merge").await?,
    ))
}

async fn decide_merge(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(run_id): Path<String>,
    Json(req): Json<MergeDecisionRequest>,
) -> ApiResult<Json<MergeDecisionResponse>> {
    let run = WorkflowRunId::from_uuid(id_of(&run_id, "run_id")?);
    let outcome = state
        .merges
        .decide(
            &state.db,
            principal.organization_id,
            run,
            &MergeDecisionInput {
                // The authenticated principal is the merger; bodies
                // never attribute actions.
                merged_by: principal.principal,
                reason: req.reason,
                external_ref: req.external_ref,
            },
        )
        .await?;
    Ok(Json(match outcome {
        hephaestus_engine::delivery::MergeOutcome::Merged => MergeDecisionResponse::Merged,
        hephaestus_engine::delivery::MergeOutcome::AlreadyMerged => {
            MergeDecisionResponse::AlreadyMerged
        }
    }))
}
