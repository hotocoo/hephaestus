//! End-to-end HTTP surface against real PostgreSQL (ADR-009).
//!
//! Covers authentication, tenant isolation, intake idempotency, error
//! mapping and both human gates driven through the API. Missing
//! HEPHAESTUS_TEST_DATABASE_URL fails loudly, like every other crate.

#![allow(clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hephaestus_api::{AppState, AuthPolicy, router};
use hephaestus_config::ApiKey;
use hephaestus_core::id::{OrganizationId, ProjectId, RepositoryId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_db::Db;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Shared durable state across scenarios; serializing keeps queue and
/// gate assertions predictable.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn test_db() -> Db {
    let url = std::env::var("HEPHAESTUS_TEST_DATABASE_URL")
        .unwrap_or_else(|_| panic!("HEPHAESTUS_TEST_DATABASE_URL must be set"));
    let db = Db::connect_with_max(&url, 8)
        .await
        .unwrap_or_else(|e| panic!("connect: {e}"));
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                db.migrate()
                    .await
                    .unwrap_or_else(|e| panic!("migrate: {e}"));
            });
        });
    });
    db
}

/// Tenant rows for one scenario; returns the identifiers clients use.
async fn seed_tenant(db: &Db) -> (OrganizationId, ProjectId, RepositoryId) {
    let org = OrganizationId::generate();
    let proj = ProjectId::generate();
    let repo = RepositoryId::generate();
    let slug = format!("api-{}", org.as_uuid().simple());
    sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
        .bind(org.as_uuid())
        .bind(&slug)
        .execute(db.pool())
        .await
        .expect("org");
    sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
        .bind(proj.as_uuid())
        .bind(org.as_uuid())
        .bind(format!("apip-{}", proj.as_uuid().simple()))
        .execute(db.pool())
        .await
        .expect("proj");
    sqlx::query(
        "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
         VALUES ($1,$2,$3,'https://example.invalid/a.git','A')",
    )
    .bind(repo.as_uuid())
    .bind(org.as_uuid())
    .bind(proj.as_uuid())
    .execute(db.pool())
    .await
    .expect("repo");
    (org, proj, repo)
}

fn key_for(token: &str, org: OrganizationId, principal: &str) -> ApiKey {
    ApiKey {
        token: token.into(),
        organization_id: org.as_uuid(),
        principal: principal.into(),
    }
}

fn app_with(db: &Db, keys: Vec<ApiKey>, disabled: bool) -> Router {
    let state = AppState::new(
        db.clone(),
        AuthPolicy::new(disabled, &keys),
        8 * 1024 * 1024,
    );
    router(state)
}

const TOKEN_A: &str = "token-a-16-chars-ok";
const TOKEN_B: &str = "token-b-16-chars-ok";

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, impl AsRef<str>)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in headers {
        builder = builder.header(*name, value.as_ref());
    }
    let request = builder
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .expect("request");
    let response = app.oneshot(request).await.expect("infallible service");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

fn bearer(token: &'static str) -> (&'static str, String) {
    ("authorization", format!("Bearer {token}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn probes_serve_without_auth() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let app = app_with(&db, vec![], false);

    let (status, body) = send(app.clone(), "GET", "/healthz", &[] as &[(&str, &str)], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");

    let (status, body) = send(app, "GET", "/readyz", &[] as &[(&str, &str)], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ready");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_or_unknown_tokens_are_unauthorized_with_public_shape() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let (org, _proj, _repo) = seed_tenant(&db).await;
    let app = app_with(&db, vec![key_for(TOKEN_A, org, "tester")], false);

    for uri in ["/api/v1/tasks", "/api/v1/projects"] {
        let empty: &[(&str, &str)] = &[];
        let (status, body) = send(app.clone(), "GET", uri, empty, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(body["code"], "UNAUTHENTICATED");
    }

    let (status, _) = send(
        app,
        "GET",
        "/api/v1/tasks",
        &[bearer("not-the-right-token-")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_auth_still_requires_explicit_tenant() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let (org, _proj, _repo) = seed_tenant(&db).await;
    let app = app_with(&db, vec![], true);
    let org_value = org.as_uuid().to_string();
    let org_header = ("x-hephaestus-organization", org_value.as_str());

    let no_headers: &[(&str, &str)] = &[];
    let (status, _) = send(app.clone(), "GET", "/api/v1/tasks", no_headers, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = send(app, "GET", "/api/v1/tasks", &[org_header], None).await;
    assert_eq!(status, StatusCode::OK);
}

/// The served document must be byte-identical to the builder: there
/// is no separately-maintained spec file to drift from.
#[tokio::test(flavor = "multi_thread")]
async fn served_contract_matches_the_builder() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let app = app_with(&db, vec![], false);
    let empty: &[(&str, &str)] = &[];
    let (status, body) = send(app, "GET", "/api/v1/openapi.json", empty, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        serde_json::to_value(hephaestus_api::openapi::openapi_document())
            .expect("document is json")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn intake_is_idempotent_and_validates_input() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let (org, proj, repo) = seed_tenant(&db).await;
    let app = app_with(&db, vec![key_for(TOKEN_A, org, "tester")], false);
    let auth = [bearer(TOKEN_A)];

    // Validation failure renders the shared error shape.
    let bad = json!({
        "title": "",
        "description": "d",
        "project_id": proj.as_uuid(),
        "repository_id": repo.as_uuid(),
        "priority": "high",
    });
    let (status, body) = send(app.clone(), "POST", "/api/v1/tasks", &auth, Some(bad)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "VALIDATION_FAILED");

    // Unknown enum values are rejected with the field named.
    let bad_priority = json!({
        "title": "t",
        "description": "d",
        "project_id": proj.as_uuid(),
        "repository_id": repo.as_uuid(),
        "priority": "urgent",
    });
    let (status, body) = send(
        app.clone(),
        "POST",
        "/api/v1/tasks",
        &auth,
        Some(bad_priority),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["field"], "priority");

    // Happy path: created once, deduplicated on replay.
    let payload = json!({
        "title": "Fix the flaky upload",
        "description": "Uploads below the size limit fail intermittently.",
        "project_id": proj.as_uuid(),
        "repository_id": repo.as_uuid(),
        "priority": "high",
        "risk": "medium",
        "labels": ["bug"],
    });
    let headers = [
        bearer(TOKEN_A),
        ("idempotency-key", "client-key-1".to_string()),
    ];
    let header_refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();

    let (status, first) = send(
        app.clone(),
        "POST",
        "/api/v1/tasks",
        &header_refs,
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(first["deduplicated"], false);

    let (status, replay) = send(
        app.clone(),
        "POST",
        "/api/v1/tasks",
        &header_refs,
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay["task_id"], first["task_id"]);
    assert_eq!(replay["run_id"], first["run_id"]);
    assert_eq!(replay["deduplicated"], true);

    // The receipt resolves: task detail and run status are readable
    // within the same tenant.
    let task_uri = format!("/api/v1/tasks/{}", first["task_id"].as_str().unwrap());
    let (status, task) = send(app.clone(), "GET", &task_uri, &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(task["title"], "Fix the flaky upload");
    assert_eq!(task["priority"], "high");

    let run_uri = format!("/api/v1/runs/{}", first["run_id"].as_str().unwrap());
    let (status, run) = send(app.clone(), "GET", &run_uri, &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["state"], "created");

    // Run events exist from bootstrapping and are provenance-tagged.
    let events_uri = format!("{}/events", run_uri);
    let (status, events) = send(app, "GET", &events_uri, &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        events
            .as_array()
            .expect("array")
            .iter()
            .all(|e| e["provenance"].is_string())
    );
}

/// Drive the legal transitions from created to awaiting_approval and
/// leave behind a current plan plus an open plan gate - exactly what
/// planning parks when it finishes.
async fn seed_to_awaiting_approval(
    db: &Db,
    org: OrganizationId,
    task: hephaestus_core::id::TaskId,
    run: hephaestus_core::id::WorkflowRunId,
) {
    let steps = [
        (WorkflowState::Created, TransitionEvent::StartAnalysis),
        (WorkflowState::Analyzing, TransitionEvent::StartPlanning),
        (WorkflowState::Planning, TransitionEvent::SubmitForApproval),
    ];
    for (from, event) in steps {
        db.transition_run(org, run, from, event, "seed")
            .await
            .expect("legal seed transition");
    }
    let plan_id = hephaestus_core::id::PlanId::generate();
    let plan = hephaestus_core::domain::Plan {
        id: plan_id,
        task_id: task,
        objective: "Fix the flaky upload.".into(),
        steps: vec![hephaestus_core::domain::PlanStep {
            id: hephaestus_core::id::StepId::generate(),
            plan_id,
            position: 1,
            action: "Adjust upload size guard".into(),
            verification: "unit:upload".into(),
            risks: vec![],
        }],
        affected_components: vec![],
        affected_symbols: vec![],
        strategy: hephaestus_core::domain::StrategyNotes::default(),
        created_at: chrono::Utc::now(),
        prompt_version: Some("test-v1".into()),
    };
    db.create_plan(org, task, run, &plan)
        .await
        .expect("seed plan");
    db.open_plan_approval(org, task, run)
        .await
        .expect("open gate");
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_tenant_access_is_indistinguishable_from_missing() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let (org_a, proj_a, repo_a) = seed_tenant(&db).await;
    let (org_b, _proj_b, _repo_b) = seed_tenant(&db).await;

    // Org A submits work; org B holds its own valid key.
    let app = app_with(
        &db,
        vec![
            key_for(TOKEN_A, org_a, "tester-a"),
            key_for(TOKEN_B, org_b, "tester-b"),
        ],
        false,
    );

    let payload = json!({
        "title": "org a secret task",
        "description": "d",
        "project_id": proj_a.as_uuid(),
        "repository_id": repo_a.as_uuid(),
    });
    let auth_a = [bearer(TOKEN_A)];
    let (_status, receipt) =
        send(app.clone(), "POST", "/api/v1/tasks", &auth_a, Some(payload)).await;

    // B cannot read A's run...
    let run_uri = format!("/api/v1/runs/{}", receipt["run_id"].as_str().unwrap());
    let (status, body) = send(app.clone(), "GET", &run_uri, &[bearer(TOKEN_B)], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");

    // ...nor A's task, nor its events.
    let task_uri = format!("/api/v1/tasks/{}", receipt["task_id"].as_str().unwrap());
    let (status, _) = send(app.clone(), "GET", &task_uri, &[bearer(TOKEN_B)], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let events_uri = format!("{}/events", run_uri);
    let (status, events) = send(app, "GET", &events_uri, &[bearer(TOKEN_B)], None).await;
    // Listings render emptiness instead of existence leaks.
    assert_eq!(status, StatusCode::OK);
    assert!(events.as_array().expect("array").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog_listings_are_scoped_and_filterable() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let (org_a, proj_a, repo_a) = seed_tenant(&db).await;
    let (org_b, _proj_b, _repo_b) = seed_tenant(&db).await;
    let app = app_with(
        &db,
        vec![
            key_for(TOKEN_A, org_a, "tester-a"),
            key_for(TOKEN_B, org_b, "tester-b"),
        ],
        false,
    );

    let (status, projects) = send(
        app.clone(),
        "GET",
        "/api/v1/projects",
        &[bearer(TOKEN_A)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let projects = projects.as_array().expect("array");
    assert!(projects.iter().any(|p| p["id"] == json!(proj_a.as_uuid())));
    assert!(
        projects
            .iter()
            .all(|p| p["organization_id"] == json!(org_a.as_uuid()))
    );

    let repos_uri = format!("/api/v1/repositories?project_id={}", proj_a.as_uuid());
    let (status, repos) = send(app.clone(), "GET", &repos_uri, &[bearer(TOKEN_A)], None).await;
    assert_eq!(status, StatusCode::OK);
    let repos = repos.as_array().expect("array");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["id"], json!(repo_a.as_uuid()));
    assert_eq!(repos[0]["default_branch"], "main");

    // Pagination bounds apply.
    let (status, page) = send(
        app,
        "GET",
        "/api/v1/projects?limit=1&offset=100",
        &[bearer(TOKEN_A)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.as_array().expect("array").is_empty());
}

/// Walk the legal transitions from created to awaiting_merge and leave
/// behind a passed execution plus an open merge gate - exactly what
/// review parks when it finishes (mirrors the delivery-phase seeding).
async fn seed_to_awaiting_merge(
    db: &Db,
    org: OrganizationId,
    task: hephaestus_core::id::TaskId,
    run: hephaestus_core::id::WorkflowRunId,
) {
    let steps = [
        (WorkflowState::Created, TransitionEvent::StartAnalysis),
        (WorkflowState::Analyzing, TransitionEvent::StartPlanning),
        (WorkflowState::Planning, TransitionEvent::SubmitForApproval),
        (WorkflowState::AwaitingApproval, TransitionEvent::Approve),
        (
            WorkflowState::Implementing,
            TransitionEvent::StartVerification,
        ),
        (
            WorkflowState::Verifying,
            TransitionEvent::VerificationPassed,
        ),
        (WorkflowState::Reviewing, TransitionEvent::ReviewPassed),
    ];
    for (from, event) in steps {
        db.transition_run(org, run, from, event, "seed")
            .await
            .expect("legal seed transition");
    }
    let plan_id = hephaestus_core::id::PlanId::generate();
    let plan = hephaestus_core::domain::Plan {
        id: plan_id,
        task_id: task,
        objective: "Deliver the change set.".into(),
        steps: vec![hephaestus_core::domain::PlanStep {
            id: hephaestus_core::id::StepId::generate(),
            plan_id,
            position: 1,
            action: "Implement the change".into(),
            verification: "unit:change".into(),
            risks: vec![],
        }],
        affected_components: vec![],
        affected_symbols: vec![],
        strategy: hephaestus_core::domain::StrategyNotes {
            rollback: Some("revert".into()),
            deployment: None,
            verification: vec!["format".into()],
        },
        created_at: chrono::Utc::now(),
        prompt_version: Some("test-v1".into()),
    };
    db.create_plan(org, task, run, &plan)
        .await
        .expect("seed plan");
    let exec = db
        .create_execution_for_run(org, task, run, plan_id)
        .await
        .expect("seed execution");
    db.finish_execution(org, exec, "passed")
        .await
        .expect("execution passed");
    db.open_merge_gate(org, task, run)
        .await
        .expect("open merge gate");
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_gate_records_external_decision_and_replays() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let (org, proj, repo) = seed_tenant(&db).await;
    let app = app_with(&db, vec![key_for(TOKEN_A, org, "release-manager")], false);
    let auth = [bearer(TOKEN_A)];

    let payload = json!({
        "title": "merge flow",
        "description": "d",
        "project_id": proj.as_uuid(),
        "repository_id": repo.as_uuid(),
    });
    let (_status, receipt) = send(app.clone(), "POST", "/api/v1/tasks", &auth, Some(payload)).await;
    let task_id = hephaestus_core::id::TaskId::from_uuid(
        uuid::Uuid::parse_str(receipt["task_id"].as_str().unwrap()).expect("uuid"),
    );
    let run_id = hephaestus_core::id::WorkflowRunId::from_uuid(
        uuid::Uuid::parse_str(receipt["run_id"].as_str().unwrap()).expect("uuid"),
    );
    seed_to_awaiting_merge(&db, org, task_id, run_id).await;

    let merge_uri = format!("/api/v1/runs/{}/merge", receipt["run_id"].as_str().unwrap());
    let gate_uri = format!(
        "/api/v1/runs/{}/merge-gate",
        receipt["run_id"].as_str().unwrap()
    );

    // The open gate is visible and undecided.
    let (status, gate) = send(app.clone(), "GET", &gate_uri, &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gate["gate"], "merge");
    assert_eq!(gate["decision"], Value::Null);

    // Record the externally made decision; the merger comes from the
    // authenticated principal, never from the body.
    let (status, outcome) = send(
        app.clone(),
        "POST",
        &merge_uri,
        &auth,
        Some(json!({ "reason": "green ci", "external_ref": "commit:abc1234" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "merge rejected: {outcome}");
    assert_eq!(outcome["outcome"], "merged");

    // Run advanced to building with one active build row chained.
    let run_uri = format!("/api/v1/runs/{}", receipt["run_id"].as_str().unwrap());
    let (_status, run) = send(app.clone(), "GET", &run_uri, &auth, None).await;
    assert_eq!(run["state"], "building");
    let active = db
        .active_build_for_run(org, run_id)
        .await
        .expect("build row");
    assert!(active.is_some(), "merge must chain a build row");

    // Gate carries the recorded decision ('approved' is the stored
    // value for a merge that passed its gate).
    let (_status, gate) = send(app.clone(), "GET", &gate_uri, &auth, None).await;
    assert_eq!(gate["decision"], "approved");

    // Replaying the same decision absorbs silently.
    let (status, replay) = send(
        app,
        "POST",
        &merge_uri,
        &auth,
        Some(json!({ "reason": "green ci", "external_ref": "commit:abc1234" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["outcome"], "already_merged");
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_approval_gate_decides_and_replays_through_the_api() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let (org, proj, repo) = seed_tenant(&db).await;
    let app = app_with(&db, vec![key_for(TOKEN_A, org, "human-approver")], false);
    let auth = [bearer(TOKEN_A)];

    // Submit through intake so the run exists, then seed planning state.
    let payload = json!({
        "title": "approval flow",
        "description": "d",
        "project_id": proj.as_uuid(),
        "repository_id": repo.as_uuid(),
    });
    let (_status, receipt) = send(app.clone(), "POST", "/api/v1/tasks", &auth, Some(payload)).await;
    let task_id: hephaestus_core::id::TaskId = hephaestus_core::id::TaskId::from_uuid(
        uuid::Uuid::parse_str(receipt["task_id"].as_str().unwrap()).expect("uuid"),
    );
    let run_id: hephaestus_core::id::WorkflowRunId = hephaestus_core::id::WorkflowRunId::from_uuid(
        uuid::Uuid::parse_str(receipt["run_id"].as_str().unwrap()).expect("uuid"),
    );
    seed_to_awaiting_approval(&db, org, task_id, run_id).await;

    let run_uri = format!("/api/v1/runs/{}", receipt["run_id"].as_str().unwrap());
    let gate_uri = format!("{run_uri}/approval");

    // The current plan is readable before any decision.
    let task_uri = format!("/api/v1/tasks/{}", receipt["task_id"].as_str().unwrap());
    let (status, plan) = send(app.clone(), "GET", &format!("{task_uri}/plan"), &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plan["objective"], "Fix the flaky upload.");
    assert_eq!(plan["steps"][0]["action"], "Adjust upload size guard");

    // The open gate is visible and undecided.
    let (status, gate) = send(app.clone(), "GET", &gate_uri, &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gate["gate"], "plan");
    assert_eq!(gate["decision"], Value::Null);

    // Approve over HTTP; the service bootstraps execution.
    let (status, outcome) = send(
        app.clone(),
        "POST",
        &gate_uri,
        &auth,
        Some(json!({ "approved": true, "reason": "plan looks right" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approval rejected: {outcome}");
    assert_eq!(outcome["outcome"], "approved");
    assert!(outcome["execution_id"].is_string());
    assert!(outcome["enqueued_step"].is_string());

    // Gate now carries the decision; run advanced to implementing.
    let (_status, gate) = send(app.clone(), "GET", &gate_uri, &auth, None).await;
    assert_eq!(gate["decision"], "approved");
    let (_status, run) = send(app.clone(), "GET", &run_uri, &auth, None).await;
    assert_eq!(run["state"], "implementing");

    // A replayed identical decision re-derives the same outcome
    // instead of double-spending a second execution bootstrap.
    let (status, replay) = send(
        app.clone(),
        "POST",
        &gate_uri,
        &auth,
        Some(json!({ "approved": true, "reason": "plan looks right" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, outcome);

    // A different decision on the decided gate is refused loudly.
    let (status, body) = send(
        app.clone(),
        "POST",
        &gate_uri,
        &auth,
        Some(json!({ "approved": false, "reason": "changed my mind" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "CONFLICT");

    // Events recorded the decision's effect on the run (the typed
    // ApprovalDecided event itself lives on the task aggregate; the
    // run's history carries the legal transition it caused).
    let (_status, events) = send(app, "GET", &format!("{}/events", run_uri), &auth, None).await;
    let approved = events.as_array().expect("array").iter().any(|e| {
        e["payload"]["type"] == json!("workflow_state_changed")
            && e["payload"]["data"]["trigger"] == json!("approve")
    });
    assert!(approved, "approve transition missing from events: {events}");
}

/// The task->run lookup resolves the current run without knowing its
/// id - the dashboard's path from a task to live workflow state. It
/// stays tenant-scoped and honest about missing rows.
#[tokio::test(flavor = "multi_thread")]
async fn task_run_lookup_resolves_current_run() {
    let _guard = SERIAL.lock().await;
    let db = test_db().await;
    let (org_a, proj_a, repo_a) = seed_tenant(&db).await;
    let (org_b, _proj_b, _repo_b) = seed_tenant(&db).await;

    let app = app_with(
        &db,
        vec![
            key_for(TOKEN_A, org_a, "tester-a"),
            key_for(TOKEN_B, org_b, "tester-b"),
        ],
        false,
    );
    let auth_a = [bearer(TOKEN_A)];
    let auth_b = [bearer(TOKEN_B)];

    // Unknown and malformed ids fail with the shared shape.
    let missing = uuid::Uuid::now_v7().to_string();
    let (status, body) = send(
        app.clone(),
        "GET",
        &format!("/api/v1/tasks/{missing}/run"),
        &auth_a,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");

    let empty: &[(&str, &str)] = &[];
    let (status, _body) = send(
        app.clone(),
        "GET",
        "/api/v1/tasks/not-a-uuid/run",
        empty,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = send(
        app.clone(),
        "GET",
        "/api/v1/tasks/not-a-uuid/run",
        &auth_a,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["field"], "task_id");

    // Submit real work; the receipt's run is what the lookup returns.
    let payload = json!({
        "title": "Wire the run lookup",
        "description": "The dashboard needs a path from task to run.",
        "project_id": proj_a.as_uuid(),
        "repository_id": repo_a.as_uuid(),
    });
    let (status, receipt) =
        send(app.clone(), "POST", "/api/v1/tasks", &auth_a, Some(payload)).await;
    assert_eq!(status, StatusCode::CREATED);
    let task_uri = format!("/api/v1/tasks/{}", receipt["task_id"].as_str().unwrap());

    let (status, run) = send(
        app.clone(),
        "GET",
        &format!("{task_uri}/run"),
        &auth_a,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["id"], receipt["run_id"]);
    assert_eq!(run["task_id"], receipt["task_id"]);
    assert_eq!(run["state"], "created");

    // The lookup follows transitions like any other run read.
    seed_to_awaiting_approval(
        &db,
        org_a,
        hephaestus_core::id::TaskId::from_uuid(
            uuid::Uuid::parse_str(receipt["task_id"].as_str().unwrap()).expect("task uuid"),
        ),
        hephaestus_core::id::WorkflowRunId::from_uuid(
            uuid::Uuid::parse_str(receipt["run_id"].as_str().unwrap()).expect("run uuid"),
        ),
    )
    .await;
    let (_status, run) = send(
        app.clone(),
        "GET",
        &format!("{task_uri}/run"),
        &auth_a,
        None,
    )
    .await;
    assert_eq!(run["state"], "awaiting_approval");

    // Another tenant gets the same answer as everyone else: nothing.
    let (status, body) = send(app, "GET", &format!("{task_uri}/run"), &auth_b, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");
}
