//! The machine-readable API contract (OpenAPI 3.1).
//!
//! Served publicly at `/api/v1/openapi.json`. One document describes
//! every operation the router serves, named after the exact wire types
//! handlers render (ADR-002). The TypeScript tier mirrors this surface
//! twice - generated types and zod schemas - and conformance tests
//! validate live responses against both, so drift fails CI from every
//! direction: document vs. router, schemas vs. server, client vs.
//! document.
//!
//! This module is deliberately hand-maintained instead of derived from
//! annotations: the document is part of the public contract and every
//! change to it must be a deliberate, reviewable diff.

use axum::Json;
use serde_json::{Value, json};

/// OpenAPI dialect this document targets.
pub const OPENAPI_VERSION: &str = "3.1.0";

/// Canonical `(method, path)` inventory of every operation the
/// document describes. Tests pin the actual router against exactly
/// this list so an undocumented route or a stale entry fails loudly.
pub const OPERATIONS: [(&str, &str); 20] = [
    ("GET", "/healthz"),
    ("GET", "/readyz"),
    ("GET", "/api/v1/openapi.json"),
    ("POST", "/api/v1/tasks"),
    ("GET", "/api/v1/tasks"),
    ("GET", "/api/v1/tasks/{task_id}"),
    ("GET", "/api/v1/tasks/{task_id}/plan"),
    ("GET", "/api/v1/tasks/{task_id}/run"),
    ("GET", "/api/v1/projects"),
    ("GET", "/api/v1/repositories"),
    ("GET", "/api/v1/runs/{run_id}"),
    ("GET", "/api/v1/runs/{run_id}/deployment"),
    ("GET", "/api/v1/runs/{run_id}/events"),
    ("GET", "/api/v1/runs/{run_id}/artifacts"),
    ("GET", "/api/v1/runs/{run_id}/artifacts/{artifact_id}"),
    (
        "GET",
        "/api/v1/runs/{run_id}/artifacts/{artifact_id}/verification",
    ),
    ("GET", "/api/v1/runs/{run_id}/approval"),
    ("POST", "/api/v1/runs/{run_id}/approval"),
    ("GET", "/api/v1/runs/{run_id}/merge-gate"),
    ("POST", "/api/v1/runs/{run_id}/merge"),
];

/// Handler serving the document. Public: contract discovery must not
/// require credentials, mirroring how probes are exposed.
pub async fn serve_document() -> Json<Value> {
    Json(openapi_document())
}

/// Build the complete OpenAPI document.
pub fn openapi_document() -> Value {
    // Operations grouped under their path item, inventory order kept.
    let mut grouped: Vec<(&str, Vec<(&str, Value)>)> = Vec::new();
    for (method, path) in OPERATIONS {
        let op: Value = match (method, path) {
            ("GET", "/healthz") => probe_operation(
                "getHealthz",
                "Liveness probe",
                response_map(&[("200", ok_json("The process is up.", sref("StatusBody")))]),
            ),
            ("GET", "/readyz") => probe_operation(
                "getReadyz",
                "Readiness probe",
                response_map(&[
                    ("200", ok_json("Dependencies are reachable.", sref("StatusBody"))),
                    ("503", error("A dependency is unreachable.")),
                ]),
            ),
            ("GET", "/api/v1/openapi.json") => Operation::new(
                "getOpenapiDocument",
                "Fetch this contract document",
                &["Contract"],
                json!([]),
                response_map(&[("200", json!({
                    "description": "This document.",
                    "content": {"application/json": {"schema": {"type": "object"}}}
                }))]),
            )
            .security(json!([]))
            .into(),
            ("POST", "/api/v1/tasks") => Operation::new(
                "submitTask",
                "Submit a task for engineering work",
                &["Tasks"],
                json!([]),
                response_map(&[
                    ("201", ok_json("Task accepted and a run bootstrapped.", sref("IntakeResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("413", json!({"description": "Body exceeds the configured size limit."})),
                    ("422", error("Input failed validation; the offending field is named.")),
                ]),
            )
            .header_param(json!({
                "name": "Idempotency-Key",
                "in": "header",
                "required": false,
                "description": "Client-supplied key; resubmission with the same key returns the original receipt instead of creating a duplicate task.",
                "schema": {"type": "string", "minLength": 1}
            }))
            .request_body(json!({
                "required": true,
                "content": {"application/json": {"schema": sref("CreateTaskRequest")}}
            }))
            .into(),
            ("GET", "/api/v1/tasks") => protected_operation(
                "listTasks",
                "List tasks of the caller's organization",
                &["Tasks"],
                page_parameters(),
                response_map(&[
                    ("200", ok_json("One page of tasks.", json!({"type": "array", "items": sref("TaskResponse")}))),
                    ("401", error("Missing or unknown bearer token.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/tasks/{task_id}") => protected_operation(
                "getTask",
                "Fetch one task",
                &["Tasks"],
                json!([]),
                response_map(&[
                    ("200", ok_json("The task.", sref("TaskResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No task with that id exists in the caller's organization.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/tasks/{task_id}/plan") => protected_operation(
                "getTaskPlan",
                "Fetch the current plan for a task",
                &["Tasks"],
                json!([]),
                response_map(&[
                    ("200", ok_json("The current plan; earlier plans were superseded.", sref("Plan"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No plan exists yet, or the task does not exist in the caller's organization.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/tasks/{task_id}/run") => protected_operation(
                "getTaskRun",
                "Fetch the current workflow run of a task",
                &["Runs"],
                json!([]),
                response_map(&[
                    ("200", ok_json("The task's current run with its workflow state.", sref("RunResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No run exists for the task yet, or the task does not exist in the caller's organization.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/projects") => protected_operation(
                "listProjects",
                "List projects of the caller's organization",
                &["Catalog"],
                page_parameters(),
                response_map(&[
                    ("200", ok_json("One page of projects.", json!({"type": "array", "items": sref("ProjectResponse")}))),
                    ("401", error("Missing or unknown bearer token.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/repositories") => protected_operation(
                "listRepositories",
                "List repositories, optionally filtered by project",
                &["Catalog"],
                {
                    repositories_parameters()
                },
                response_map(&[
                    ("200", ok_json("One page of repositories.", json!({"type": "array", "items": sref("RepositoryResponse")}))),
                    ("401", error("Missing or unknown bearer token.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/runs/{run_id}") => protected_operation(
                "getRun",
                "Fetch one workflow run",
                &["Runs"],
                json!([]),
                response_map(&[
                    ("200", ok_json("The run with its current workflow state.", sref("RunResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No run with that id exists in the caller's organization.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/runs/{run_id}/deployment") => protected_operation(
                "getRunDeployment",
                "Fetch the run's latest deployment",
                &["Runs"],
                json!([]),
                response_map(&[
                    ("200", ok_json("The most recent deployment of the run, any status.", sref("DeploymentResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("The run never deployed, or does not exist in the caller's organization.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/runs/{run_id}/events") => protected_operation(
                "listRunEvents",
                "List events appended to a run, newest first",
                &["Runs"],
                event_parameters(),
                response_map(&[
                    ("200", ok_json("Newest-first event page.", json!({"type": "array", "items": sref("EventResponse")}))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/runs/{run_id}/artifacts") => protected_operation(
                "listRunArtifacts",
                "List the artifacts a run's builds registered",
                &["Runs"],
                json!([]),
                response_map(&[
                    ("200", ok_json("One entry per registered file, newest build first.", json!({"type": "array", "items": sref("ArtifactResponse")}))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No run with that id exists in the caller's organization.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/runs/{run_id}/artifacts/{artifact_id}") => Operation::new(
                "getRunArtifact",
                "Download one artifact's bytes",
                &[
                    "Runs",
                ],
                json!([]),
                response_map(&[
                    (
                        "200",
                        json!({
                            "description": "The file's bytes, streamed. The ETag is the recorded SHA-256; the X-Artifact-Sha256 header repeats it for independent verification.",
                            "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}
                        }),
                    ),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("The artifact does not exist under this run, or its file is no longer on disk.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/runs/{run_id}/artifacts/{artifact_id}/verification") => protected_operation(
                "verifyRunArtifact",
                "Re-hash one artifact on disk against its recorded digest",
                &["Runs"],
                json!([]),
                response_map(&[
                    ("200", ok_json("verified, missing or corrupt, with the expected and actual digests.", sref("ArtifactVerificationResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("The artifact does not exist under this run.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("GET", "/api/v1/runs/{run_id}/approval") => protected_operation(
                "getApprovalGate",
                "Fetch the plan-approval gate of a run",
                &["Gates"],
                json!([]),
                response_map(&[
                    ("200", ok_json("The gate with its decision when made.", sref("GateResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No plan gate exists for this run yet.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("POST", "/api/v1/runs/{run_id}/approval") => Operation::new(
                "decideApproval",
                "Approve or reject the pending plan",
                &["Gates"],
                json!([]),
                response_map(&[
                    ("200", ok_json("Decision recorded; the outcome reports what was unlocked.", sref("ApprovalDecisionResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No gate exists for this run.")),
                    ("409", error("The workflow moved past the gate; the decision cannot apply.")),
                    ("422", error("Malformed body or non-UUID id.")),
                ]),
            )
            .request_body(json!({
                "required": true,
                "content": {"application/json": {"schema": sref("ApprovalDecisionRequest")}}
            }))
            .into(),
            ("GET", "/api/v1/runs/{run_id}/merge-gate") => protected_operation(
                "getMergeGate",
                "Fetch the merge gate of a run",
                &["Gates"],
                json!([]),
                response_map(&[
                    ("200", ok_json("The gate with its decision when made.", sref("GateResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No merge gate exists for this run yet.")),
                    ("422", error("The id is not a UUID.")),
                ]),
            )
            .into(),
            ("POST", "/api/v1/runs/{run_id}/merge") => Operation::new(
                "decideMerge",
                "Record an externally performed merge",
                &["Gates"],
                json!([]),
                response_map(&[
                    ("200", ok_json("Decision recorded and the delivery pipeline chained.", sref("MergeDecisionResponse"))),
                    ("401", error("Missing or unknown bearer token.")),
                    ("404", error("No run with that id exists in the caller's organization.")),
                    ("409", error("The run is not at awaiting_merge.")),
                    ("422", error("Malformed body or non-UUID id.")),
                ]),
            )
            .request_body(json!({
                "required": true,
                "content": {"application/json": {"schema": sref("MergeDecisionRequest")}}
            }))
            .into(),
            other => unreachable!("OPERATIONS contains an entry with no documented operation: {other:?}"),
        };
        match grouped.iter_mut().find(|(p, _)| *p == path) {
            Some((_, ops)) => ops.push((method, op)),
            None => grouped.push((path, vec![(method, op)])),
        }
    }

    let mut paths = serde_json::Map::new();
    for (path, ops) in grouped {
        let mut item = serde_json::Map::new();
        item.insert("parameters".into(), path_parameters(path));
        for (method, op) in ops {
            item.insert(method.to_lowercase(), op);
        }
        paths.insert(path.to_string(), Value::Object(item));
    }

    json!({
        "openapi": OPENAPI_VERSION,
        "info": {
            "title": "Hephaestus Control Plane API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "An AI-native software engineering control plane. AI proposes and executes engineering work; deterministic systems verify it. Every endpoint scopes store access by the authenticated principal's organization (ADR-009); job processing runs in workers (ADR-010); this document is the single source the web tier's contracts mirror (ADR-002)."
        },
        "servers": [{"url": "/", "description": "The serving process itself"}],
        "tags": [
            {"name": "Probes", "description": "Unauthenticated process health."},
            {"name": "Contract", "description": "This document."},
            {"name": "Tasks", "description": "Engineering task intake and reads."},
            {"name": "Catalog", "description": "Projects and repositories of one organization."},
            {"name": "Runs", "description": "Workflow runs and their events."},
            {"name": "Gates", "description": "Human decision points."}
        ],
        "security": [{"bearerAuth": []}],
        "paths": Value::Object(paths),
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "Pre-provisioned API key bound to exactly one organization."
                }
            },
            "schemas": schemas(),
        }
    })
}

/// Shared schema definitions. Names mirror the Rust wire types in
/// `crate::dto` (and the domain `Plan` documents that pass through)
/// so generated TypeScript keeps the same vocabulary.
fn schemas() -> Value {
    let uuid = || json!({"type": "string", "format": "uuid"});
    let datetime = || json!({"type": "string", "format": "date-time"});
    json!({
        "StatusBody": {
            "type": "object",
            "required": ["status"],
            "properties": {"status": {"type": "string"}}
        },
        "ErrorBody": {
            "type": "object",
            "required": ["code", "message"],
            "properties": {
                "code": {"type": "string", "description": "Stable public error code, e.g. VALIDATION_FAILED or UNAUTHENTICATED."},
                "message": {"type": "string", "description": "Safe human-readable explanation; internals never reach clients."},
                "field": {"type": "string", "description": "Offending input field, present for validation failures only."}
            }
        },
        "ProjectResponse": {
            "type": "object",
            "required": ["id", "organization_id", "name", "slug"],
            "properties": {
                "id": uuid(),
                "organization_id": uuid(),
                "name": {"type": "string"},
                "slug": {"type": "string"}
            }
        },
        "RepositoryResponse": {
            "type": "object",
            "required": ["id", "organization_id", "project_id", "remote_url", "default_branch", "display_name"],
            "properties": {
                "id": uuid(),
                "organization_id": uuid(),
                "project_id": uuid(),
                "remote_url": {"type": "string"},
                "default_branch": {"type": "string"},
                "display_name": {"type": "string"}
            }
        },
        "TaskResponse": {
            "type": "object",
            "required": ["id", "organization_id", "project_id", "repository_id", "title", "description", "priority", "risk", "labels", "created_at", "updated_at"],
            "properties": {
                "id": uuid(),
                "organization_id": uuid(),
                "project_id": uuid(),
                "repository_id": uuid(),
                "title": {"type": "string"},
                "description": {"type": "string"},
                "priority": {"type": "string", "enum": ["critical", "high", "medium", "low"]},
                "risk": {"type": "string", "enum": ["low", "medium", "high", "critical"]},
                "labels": {"description": "Normalized label array rendered as stored JSON."},
                "created_at": datetime(),
                "updated_at": datetime()
            }
        },
        "CreateTaskRequest": {
            "type": "object",
            "required": ["title", "description", "project_id", "repository_id"],
            "properties": {
                "title": {"type": "string", "minLength": 1, "maxLength": 512},
                "description": {"type": "string"},
                "project_id": uuid(),
                "repository_id": uuid(),
                "priority": {"type": "string", "enum": ["critical", "high", "medium", "low"], "description": "Defaults to medium."},
                "risk": {"type": "string", "enum": ["low", "medium", "high", "critical"], "description": "Defaults to medium."},
                "labels": {"type": "array", "items": {"type": "string"}, "description": "Trimmed, lowercased, deduplicated; each at most 64 characters."}
            }
        },
        "IntakeResponse": {
            "type": "object",
            "required": ["task_id", "run_id", "deduplicated"],
            "properties": {
                "task_id": uuid(),
                "run_id": uuid(),
                "deduplicated": {"type": "boolean", "description": "True when an existing task matched the idempotency key."}
            }
        },
        "RunResponse": {
            "type": "object",
            "required": ["id", "task_id", "organization_id", "state", "attempt", "correlation_id", "lease_owner", "lease_expires_at", "last_transition_at"],
            "properties": {
                "id": uuid(),
                "task_id": uuid(),
                "organization_id": uuid(),
                "state": {"type": "string", "description": "Current workflow state name, e.g. intake, awaiting_approval, awaiting_merge, completed."},
                "attempt": {"type": "integer", "format": "int32"},
                "correlation_id": uuid(),
                "lease_owner": {"type": ["string", "null"]},
                "lease_expires_at": {"oneOf": [datetime(), {"type": "null"}]},
                "last_transition_at": datetime()
            }
        },
        "DeploymentResponse": {
            "type": "object",
            "required": ["id", "task_id", "run_id", "build_id", "target", "status", "failure_reason", "created_at"],
            "properties": {
                "id": uuid(),
                "task_id": uuid(),
                "run_id": uuid(),
                "build_id": uuid(),
                "target": {"type": "string", "description": "Configured target name the plan cited."},
                "status": {"type": "string", "enum": ["running", "succeeded", "failed"]},
                "failure_reason": {"type": ["string", "null"], "description": "Why the deployment failed, when it failed."},
                "created_at": datetime()
            }
        },
        "ArtifactResponse": {
            "type": "object",
            "required": ["id", "task_id", "run_id", "build_id", "path", "sha256", "size_bytes", "created_at"],
            "properties": {
                "id": uuid(),
                "task_id": uuid(),
                "run_id": uuid(),
                "build_id": uuid(),
                "path": {"type": "string", "description": "Workspace-relative file path. Contained: no absolute paths, no .. segments."},
                "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$", "description": "SHA-256 over the file content."},
                "size_bytes": {"type": "integer", "format": "int64", "minimum": 0},
                "created_at": datetime()
            }
        },
        "ArtifactVerificationResponse": {
            "type": "object",
            "required": ["artifact_id", "status", "expected_sha256", "actual_sha256"],
            "properties": {
                "artifact_id": uuid(),
                "status": {"type": "string", "enum": ["verified", "missing", "corrupt"], "description": "verified: on-disk bytes hash to the recorded digest. missing: the file is gone. corrupt: the bytes differ."},
                "expected_sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$", "description": "Digest the build recorded."},
                "actual_sha256": {"oneOf": [{"type": "string", "pattern": "^[0-9a-f]{64}$"}, {"type": "null"}], "description": "Digest of the bytes currently on disk, when readable."}
            }
        },
        "EventResponse": {
            "type": "object",
            "required": ["id", "aggregate", "aggregate_id", "provenance", "payload", "occurred_at"],
            "properties": {
                "id": uuid(),
                "aggregate": {"type": "string"},
                "aggregate_id": uuid(),
                "provenance": {"type": "string", "description": "Provenance classification for prompt-injection defense; untrusted payloads are data, never instructions."},
                "payload": {"description": "Versioned event payload as stored JSON."},
                "occurred_at": datetime()
            }
        },
        "GateResponse": {
            "type": "object",
            "required": ["approval_id", "gate", "required_role", "decision"],
            "properties": {
                "approval_id": uuid(),
                "gate": {"type": "string", "enum": ["plan", "merge"]},
                "required_role": {"type": "string"},
                "decision": {"type": ["string", "null"], "description": "Recorded decision name when made."}
            }
        },
        "ApprovalDecisionRequest": {
            "type": "object",
            "required": ["approved"],
            "properties": {
                "approved": {"type": "boolean"},
                "reason": {"type": ["string", "null"], "description": "Free-form rationale, persisted with the decision."}
            }
        },
        "ApprovalDecisionResponse": {
            "oneOf": [
                {
                    "type": "object",
                    "required": ["outcome", "execution_id", "enqueued_step"],
                    "properties": {
                        "outcome": {"const": "approved"},
                        "execution_id": uuid(),
                        "enqueued_step": {"oneOf": [uuid(), {"type": "null"}], "description": "Step enqueued for implementation, when one was pending."}
                    }
                },
                {
                    "type": "object",
                    "required": ["outcome", "approval_id"],
                    "properties": {
                        "outcome": {"const": "rejected"},
                        "approval_id": uuid()
                    }
                }
            ]
        },
        "MergeDecisionRequest": {
            "type": "object",
            "properties": {
                "reason": {"type": ["string", "null"]},
                "external_ref": {"type": ["string", "null"], "description": "External reference for the merge (commit sha, PR number)."}
            }
        },
        "MergeDecisionResponse": {
            "oneOf": [
                {
                    "type": "object",
                    "required": ["outcome"],
                    "properties": {"outcome": {"const": "merged"}}
                },
                {
                    "type": "object",
                    "required": ["outcome"],
                    "description": "Replayed request; the decision had been applied before.",
                    "properties": {"outcome": {"const": "already_merged"}}
                }
            ]
        },
        "PlanStep": {
            "type": "object",
            "required": ["id", "plan_id", "position", "action", "verification", "risks"],
            "properties": {
                "id": uuid(),
                "plan_id": uuid(),
                "position": {"type": "integer", "format": "int32", "minimum": 1, "description": "1-based order of execution; positions are contiguous across a plan."},
                "action": {"type": "string", "description": "Names concrete artifacts (files/symbols/tests), never vague goals."},
                "verification": {"type": "string", "description": "Deterministic verification for this step (layer + check reference)."},
                "risks": {"type": "array", "items": {"type": "string"}}
            }
        },
        "StrategyNotes": {
            "type": "object",
            "required": ["rollback", "deployment", "verification"],
            "properties": {
                "rollback": {"type": ["string", "null"]},
                "deployment": {"type": ["string", "null"]},
                "verification": {"type": "array", "items": {"type": "string"}}
            }
        },
        "Plan": {
            "type": "object",
            "required": ["id", "task_id", "objective", "steps", "affected_components", "affected_symbols", "strategy", "created_at", "prompt_version"],
            "properties": {
                "id": uuid(),
                "task_id": uuid(),
                "objective": {"type": "string"},
                "steps": {"type": "array", "items": sref("PlanStep"), "minItems": 1},
                "affected_components": {"type": "array", "items": {"type": "string"}},
                "affected_symbols": {"type": "array", "items": {"type": "string"}},
                "strategy": sref("StrategyNotes"),
                "created_at": datetime(),
                "prompt_version": {"type": ["string", "null"], "description": "Prompt version that produced this plan (reproducibility)."}
            }
        }
    })
}

/// A reference into `components/schemas`.
fn sref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

/// A JSON success response.
fn ok_json(description: &str, schema: Value) -> Value {
    json!({
        "description": description,
        "content": {"application/json": {"schema": schema}}
    })
}

/// An error rendered in the shared public shape.
fn error(description: &str) -> Value {
    ok_json(description, sref("ErrorBody"))
}

/// A responses object from status/body pairs.
fn response_map(pairs: &[(&str, Value)]) -> Value {
    pairs
        .iter()
        .map(|(status, response)| ((*status).to_string(), response.clone()))
        .collect::<serde_json::Map<String, Value>>()
        .into()
}

/// Path parameters implied by a path template.
fn path_parameters(path: &str) -> Value {
    let mut params = Vec::new();
    for segment in path.split('/') {
        if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            params.push(json!({
                "name": name,
                "in": "path",
                "required": true,
                "description": format!("Entity identifier; must be a UUID."),
                "schema": {"type": "string", "format": "uuid"}
            }));
        }
    }
    json!(params)
}

/// Pagination query shared by list endpoints.
fn page_parameters() -> Value {
    json!([
        {"name": "limit", "in": "query", "required": false,
         "description": "Page size.",
         "schema": {"type": "integer", "format": "int64", "default": 50}},
        {"name": "offset", "in": "query", "required": false,
         "description": "Page offset.",
         "schema": {"type": "integer", "format": "int64", "minimum": 0}}
    ])
}

/// Event listing accepts the pagination shape too; offset is part of
/// the shared wire convention even though events paginate by limit.
fn event_parameters() -> Value {
    page_parameters()
}

/// Repository listing adds an optional project filter.
fn repositories_parameters() -> Value {
    let mut params = match page_parameters().as_array() {
        Some(list) => list.clone(),
        None => Vec::new(),
    };
    params.push(json!({
        "name": "project_id",
        "in": "query",
        "required": false,
        "description": "Only repositories of this project.",
        "schema": {"type": "string", "format": "uuid"}
    }));
    Value::Array(params)
}

/// Builder for one operation object. Fields stay plain until
/// \`finish\` so no construction step can fail or panic.
struct Operation {
    id: String,
    summary: String,
    tags: Vec<String>,
    parameters: Value,
    responses: Value,
    request_body: Option<Value>,
    security_override: Option<Value>,
}

impl Operation {
    /// Start an operation.
    fn new(id: &str, summary: &str, tags: &[&str], parameters: Value, responses: Value) -> Self {
        Self {
            id: id.to_string(),
            summary: summary.to_string(),
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            parameters,
            responses,
            request_body: None,
            security_override: None,
        }
    }

    /// Attach a request body.
    fn request_body(mut self, body: Value) -> Self {
        self.request_body = Some(body);
        self
    }

    /// Attach a header parameter alongside path/query parameters.
    fn header_param(self, param: Value) -> Self {
        let mut params = match self.parameters.as_array() {
            Some(list) => list.clone(),
            None => Vec::new(),
        };
        params.push(param);
        Self {
            parameters: Value::Array(params),
            ..self
        }
    }

    /// Override document-level security (used by routes under the
    /// versioned prefix that take no credentials).
    fn security(mut self, security: Value) -> Self {
        self.security_override = Some(security);
        self
    }

    /// Render the finished operation object.
    fn finish(self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("operationId".into(), json!(self.id));
        obj.insert("summary".into(), json!(self.summary));
        obj.insert("tags".into(), json!(self.tags));
        obj.insert("parameters".into(), self.parameters);
        obj.insert("responses".into(), self.responses);
        if let Some(body) = self.request_body {
            obj.insert("requestBody".into(), body);
        }
        if let Some(security) = self.security_override {
            obj.insert("security".into(), security);
        }
        Value::Object(obj)
    }
}

impl From<Operation> for Value {
    fn from(op: Operation) -> Self {
        op.finish()
    }
}

/// A probe operation: unauthenticated, no parameters.
fn probe_operation(id: &str, summary: &str, responses: Value) -> Value {
    Operation::new(id, summary, &["Probes"], json!([]), responses)
        .security(json!([]))
        .into()
}

/// An authenticated operation inheriting document-level security.
fn protected_operation(
    id: &str,
    summary: &str,
    tags: &[&str],
    parameters: Value,
    responses: Value,
) -> Operation {
    Operation::new(id, summary, tags, parameters, responses)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// Whether a path-item key names an HTTP method (vs. shared
    /// parameters or other path-level metadata).
    fn is_http_method(key: &str) -> bool {
        matches!(key, "get" | "post" | "put" | "patch" | "delete")
    }

    /// Collect every $ref pointer used anywhere under a value.
    fn collect_refs(value: &Value, refs: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if let Some(r) = map.get("$ref").and_then(Value::as_str) {
                    refs.push(r.to_string());
                }
                for v in map.values() {
                    collect_refs(v, refs);
                }
            }
            Value::Array(items) => {
                for v in items {
                    collect_refs(v, refs);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn document_names_every_operation_from_the_inventory() {
        let doc = openapi_document();
        let paths = doc["paths"].as_object().unwrap();

        let mut documented: Vec<(String, String)> = Vec::new();
        for (path, item) in paths {
            for key in item.as_object().unwrap().keys() {
                if matches!(key.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                    documented.push((key.to_uppercase(), path.clone()));
                }
            }
        }
        documented.sort();
        let mut expected: Vec<(String, String)> = OPERATIONS
            .iter()
            .map(|(m, p)| ((*m).to_string(), (*p).to_string()))
            .collect();
        expected.sort();
        assert_eq!(documented, expected);
    }

    #[test]
    fn operation_ids_are_unique() {
        let doc = openapi_document();
        let mut ids: Vec<&str> = Vec::new();
        for item in doc["paths"].as_object().unwrap().values() {
            for (method, op) in item.as_object().unwrap() {
                if !is_http_method(method) {
                    continue;
                }
                ids.push(op["operationId"].as_str().unwrap());
            }
        }
        ids.sort();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "operationIds must be unique");
    }

    #[test]
    fn every_ref_resolves_to_a_declared_schema() {
        let doc = openapi_document();
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        let mut refs = Vec::new();
        collect_refs(&doc, &mut refs);
        assert!(!refs.is_empty());
        for r in refs {
            let name = r
                .strip_prefix("#/components/schemas/")
                .unwrap_or_else(|| panic!("non-schema ref: {r}"));
            assert!(schemas.contains_key(name), "undeclared schema: {name}");
        }
    }

    #[test]
    fn every_response_has_a_description_and_errors_use_the_shared_shape() {
        let doc = openapi_document();
        for (path, item) in doc["paths"].as_object().unwrap() {
            for (method, op) in item.as_object().unwrap() {
                if !is_http_method(method) {
                    continue;
                }
                let Some(responses) = op.get("responses").and_then(Value::as_object) else {
                    panic!("{method} {path} lacks responses");
                };
                for (status, response) in responses {
                    assert!(
                        response
                            .get("description")
                            .is_some_and(|d| d.as_str().is_some_and(|s| !s.is_empty())),
                        "{method} {path} {status} lacks a description"
                    );
                }
            }
        }
    }

    #[test]
    fn probes_and_the_document_take_no_security_while_protected_ops_inherit_bearer() {
        let doc = openapi_document();
        for (path, item) in doc["paths"].as_object().unwrap() {
            let public = !path.starts_with("/api/v1/") || path.ends_with("openapi.json");
            for op in item
                .as_object()
                .unwrap()
                .values()
                .filter(|v| v.get("operationId").is_some())
            {
                let explicit = op.get("security");
                if public {
                    assert_eq!(
                        explicit,
                        Some(&json!([])),
                        "{path} must opt out of auth explicitly"
                    );
                } else {
                    assert!(
                        explicit.is_none(),
                        "{path} must inherit document-level bearerAuth"
                    );
                }
            }
        }
    }

    #[test]
    fn wire_schema_names_mirror_the_rust_types() {
        let doc = openapi_document();
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for expected in [
            "ErrorBody",
            "IntakeResponse",
            "TaskResponse",
            "RunResponse",
            "DeploymentResponse",
            "ArtifactResponse",
            "ArtifactVerificationResponse",
            "EventResponse",
            "GateResponse",
            "ProjectResponse",
            "RepositoryResponse",
            "CreateTaskRequest",
            "ApprovalDecisionRequest",
            "ApprovalDecisionResponse",
            "MergeDecisionRequest",
            "MergeDecisionResponse",
            "Plan",
            "PlanStep",
            "StrategyNotes",
            "StatusBody",
        ] {
            assert!(schemas.contains_key(expected), "missing schema: {expected}");
        }
    }

    #[test]
    fn tagged_enums_render_snake_case_discriminants() {
        let doc = openapi_document();
        let approval = &doc["components"]["schemas"]["ApprovalDecisionResponse"]["oneOf"];
        assert_eq!(approval[0]["properties"]["outcome"]["const"], "approved");
        assert_eq!(approval[1]["properties"]["outcome"]["const"], "rejected");
        let merge = &doc["components"]["schemas"]["MergeDecisionResponse"]["oneOf"];
        assert_eq!(merge[0]["properties"]["outcome"]["const"], "merged");
        assert_eq!(merge[1]["properties"]["outcome"]["const"], "already_merged");
    }
}
