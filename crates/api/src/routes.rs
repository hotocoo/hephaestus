//! Route table and handlers.
//!
//! Handlers orchestrate nothing: they authenticate through the shared
//! middleware, scope every store/service call by the caller's
//! organization, and translate between wire types and service inputs.
//! All durable effects flow through engine services so their
//! idempotency and audit guarantees hold verbatim over HTTP.

use std::path::{Path as StdPath, PathBuf};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::routing::post;
use axum::{Json, Router};
use hephaestus_core::Error;
use hephaestus_core::domain::Plan;
use hephaestus_core::event::AggregateKind;
use hephaestus_core::id::{
    ArtifactId, OrganizationId, ProjectId, RepositoryId, TaskId, WorkflowRunId,
};
use hephaestus_engine::approval::ApprovalDecisionInput;
use hephaestus_engine::delivery::MergeDecisionInput;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;
use tower_http::timeout::TimeoutLayer;
use uuid::Uuid;

use crate::auth::auth_middleware;
use crate::dto::{
    ApprovalDecisionRequest, ApprovalDecisionResponse, ArtifactResponse,
    ArtifactVerificationResponse, Authed, CreateTaskRequest, DeploymentResponse, EventResponse,
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
        .route("/runs/{run_id}/deployment", get(get_run_deployment))
        .route("/runs/{run_id}/events", get(list_run_events))
        .route("/runs/{run_id}/artifacts", get(list_run_artifacts))
        .route(
            "/runs/{run_id}/artifacts/{artifact_id}",
            get(download_artifact),
        )
        .route(
            "/runs/{run_id}/artifacts/{artifact_id}/verification",
            get(verify_artifact),
        )
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

    let mut probes = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz));
    if state.web_site.is_some() {
        // With dashboard serving enabled the SPA entry answers GET /
        // exactly like every other client-side route does.
        probes = probes.route("/", get(serve_web_entry));
    }

    probes
        .nest("/api/v1", contract.merge(protected))
        .fallback(web_or_not_found)
        .layer(DefaultBodyLimit::max(state.max_body_bytes))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            state.request_timeout(),
        ))
        .with_state(state)
}

/// Unknown paths: static SPA fallback when dashboard serving is on,
/// the shared JSON error shape otherwise - /api keeps its contract.
async fn web_or_not_found(State(state): State<AppState>, method: Method, uri: Uri) -> Response {
    let Some(site) = state.web_site.as_ref() else {
        return ApiError(Error::NotFound { entity: "route" }).into_response();
    };
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError(Error::NotFound { entity: "route" }).into_response();
    }
    site.respond(&method, path).await
}

/// The dashboard entry point at its canonical root.
async fn serve_web_entry(State(state): State<AppState>, method: Method) -> Response {
    match state.web_site.as_ref() {
        Some(site) => site.respond(&method, "/").await,
        None => ApiError(Error::NotFound { entity: "route" }).into_response(),
    }
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

/// The run's latest deployment (any status); the dashboard's view of
/// what shipped. 404 when the run never deployed - indistinguishable
/// from a missing entity, exactly like every other scoped read.
async fn get_run_deployment(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(run_id): Path<String>,
) -> ApiResult<Json<DeploymentResponse>> {
    let row = state
        .db
        .latest_deployment_for_run(
            principal.organization_id,
            WorkflowRunId::from_uuid(id_of(&run_id, "run_id")?),
        )
        .await?
        .ok_or(Error::NotFound {
            entity: "deployment",
        })?;
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

// ---------------------------------------------------------------------
// Artifact registry and serving (ADR-015).
//
// Three tenant-scoped reads over the per-file evidence a successful
// build recorded: a listing, a byte-faithful stream, and an explicit
// re-hash. The API reads the worker's storage root; without one it
// fails loudly instead of pretending.

/// The configured storage root, or a loud configuration failure.
fn storage_root(state: &AppState) -> ApiResult<&StdPath> {
    state.storage_root.as_deref().ok_or_else(|| {
        ApiError(Error::Config(
            "storage root is not configured; artifact endpoints are unavailable".into(),
        ))
    })
}

/// Resolve and containment-check one artifact's on-disk file.
///
/// `Ok(None)` means the file is absent (a `missing` verification, a
/// 404 download). `Ok(Some(path))` is the canonicalized file, proven
/// to stay inside the run workspace even through symlinks. Errors are
/// configuration faults and escape attempts.
fn resolve_artifact_file(
    root: &StdPath,
    run: Uuid,
    artifact_path: &str,
) -> ApiResult<Option<PathBuf>> {
    if artifact_path.is_empty()
        || artifact_path.starts_with('/')
        || artifact_path.split('/').any(|segment| segment == "..")
    {
        return Err(ApiError(Error::Validation {
            field: "path".into(),
            message: "artifact path escapes the run workspace".into(),
        }));
    }
    let workspace = root.join("runs").join(run.to_string()).join("repo");
    let candidate = workspace.join(artifact_path);
    if !candidate.exists() {
        return Ok(None);
    }
    let workspace_c = workspace
        .canonicalize()
        .map_err(|_| ApiError(Error::NotFound { entity: "artifact" }))?;
    let candidate_c = candidate
        .canonicalize()
        .map_err(|_| ApiError(Error::NotFound { entity: "artifact" }))?;
    if !candidate_c.starts_with(&workspace_c) {
        return Err(ApiError(Error::Validation {
            field: "path".into(),
            message: "artifact path escapes the run workspace".into(),
        }));
    }
    Ok(Some(candidate_c))
}

/// Fetch one artifact row, scoped to the caller's tenant and the run
/// named in the URL; a row under a different run is not found.
async fn artifact_for_run(
    state: &AppState,
    org: OrganizationId,
    run_id: &str,
    artifact_id: &str,
) -> ApiResult<(Uuid, hephaestus_db::delivery::ArtifactRow)> {
    let run = WorkflowRunId::from_uuid(id_of(run_id, "run_id")?);
    let artifact = ArtifactId::from_uuid(id_of(artifact_id, "artifact_id")?);
    let row = state
        .db
        .get_artifact(org, artifact)
        .await?
        .ok_or(ApiError(Error::NotFound { entity: "artifact" }))?;
    if row.run_id != run.as_uuid() {
        return Err(ApiError(Error::NotFound { entity: "artifact" }));
    }
    Ok((run.as_uuid(), row))
}

/// The registry listing for a run; the run must exist in the caller's
/// tenant. An empty list is the honest answer for runs that never
/// built.
async fn list_run_artifacts(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(run_id): Path<String>,
) -> ApiResult<Json<Vec<ArtifactResponse>>> {
    let run = WorkflowRunId::from_uuid(id_of(&run_id, "run_id")?);
    // get_run errors NotFound when the run is absent from this tenant.
    state.db.get_run(principal.organization_id, run).await?;
    let rows = state
        .db
        .list_artifacts(principal.organization_id, run)
        .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// Stream one artifact's bytes. The ETag is the recorded SHA-256 and
/// the `X-Artifact-Sha256` header lets clients verify independently.
/// A row whose file vanished is a 404, never empty bytes.
async fn download_artifact(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path((run_id, artifact_id)): Path<(String, String)>,
) -> Response {
    match download_artifact_inner(&state, principal.organization_id, run_id, artifact_id).await {
        Ok(response) => response,
        Err(err) => err.into_response(),
    }
}

async fn download_artifact_inner(
    state: &AppState,
    org: OrganizationId,
    run_id: String,
    artifact_id: String,
) -> ApiResult<Response> {
    let (run, row) = artifact_for_run(state, org, &run_id, &artifact_id).await?;
    let root = storage_root(state)?;
    let Some(file) = resolve_artifact_file(root, run, &row.path)? else {
        return Err(ApiError(Error::NotFound { entity: "artifact" }));
    };
    let size = std::fs::metadata(&file)
        .map(|m| m.len())
        .map_err(|_| ApiError(Error::NotFound { entity: "artifact" }))?;
    let file = tokio::fs::File::open(&file)
        .await
        .map_err(|_| ApiError(Error::NotFound { entity: "artifact" }))?;
    let body = Body::from_stream(ReaderStream::new(file));

    // The digest headers are always valid (hex plus ETag quotes); the
    // disposition is attached only when the filename is header-safe.
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::ETAG,
        header_value(&format!("\"{}\"", row.sha256))?,
    );
    headers.insert("x-artifact-sha256", header_value(&row.sha256)?);
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        header_value(&size.to_string())?,
    );
    let filename = row
        .path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("artifact");
    if let Ok(disposition) = header_value(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(axum::http::header::CONTENT_DISPOSITION, disposition);
    }

    Ok((StatusCode::OK, headers, body).into_response())
}

/// Parse a header value from a string that is known-valid for our
/// inputs (hex digests, numeric sizes, quoted ETags). The error path
/// is unreachable for those inputs but is mapped to an internal fault
/// rather than a panic, keeping production code panic-free.
fn header_value(raw: &str) -> ApiResult<HeaderValue> {
    HeaderValue::from_str(raw).map_err(|_| {
        ApiError(Error::Storage(Box::new(std::io::Error::other(
            "unreachable: invalid artifact header value",
        ))))
    })
}

/// Server-side re-hash of one artifact's on-disk bytes against its
/// recorded digest. A read-only statement about the disk: `verified`,
/// `missing`, or `corrupt`, never a mutation.
async fn verify_artifact(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path((run_id, artifact_id)): Path<(String, String)>,
) -> ApiResult<Json<ArtifactVerificationResponse>> {
    let (run, row) =
        artifact_for_run(&state, principal.organization_id, &run_id, &artifact_id).await?;
    let root = storage_root(&state)?;
    let file = resolve_artifact_file(root, run, &row.path)?;
    let (status, actual) = match file {
        None => ("missing".to_string(), None),
        Some(path) => {
            let mut file = tokio::fs::File::open(&path)
                .await
                .map_err(|e| Error::Storage(Box::new(e)))?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 8192];
            loop {
                let n = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| Error::Storage(Box::new(e)))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            let digest = hex::encode(hasher.finalize());
            let status = if digest == row.sha256 {
                "verified"
            } else {
                "corrupt"
            };
            (status.to_string(), Some(digest))
        }
    };
    Ok(Json(ArtifactVerificationResponse {
        artifact_id: row.id,
        status,
        expected_sha256: row.sha256,
        actual_sha256: actual,
    }))
}
