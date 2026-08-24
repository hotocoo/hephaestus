//! Engine integration tests against real PostgreSQL.
//!
//! Requires HEPHAESTUS_TEST_DATABASE_URL. These tests exercise the
//! intake->queue->worker cycle end-to-end with a recording test
//! handler standing in for repository analysis (Phase 3 replaces it
//! with the real implementation).

#![allow(clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use hephaestus_core::domain::{Priority, RiskLevel};
use hephaestus_core::id::{OrganizationId, ProjectId, RepositoryId};
use hephaestus_db::Db;
use hephaestus_engine::intake::{IntakeRequest, IntakeService};
use hephaestus_engine::jobs::JobPayload;
use hephaestus_engine::worker::{
    HandlerOutcome, HandlerRegistry, JobHandler, Worker, WorkerConfig,
};
use tokio_util::sync::CancellationToken;

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

/// Seed org/project/repo rows and return their ids.
async fn seed(db: &Db) -> (OrganizationId, ProjectId, RepositoryId) {
    let org = OrganizationId::generate();
    let proj = ProjectId::generate();
    let repo = RepositoryId::generate();
    let slug = format!("eng-{}", org.as_uuid().simple());
    sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
        .bind(org.as_uuid())
        .bind(&slug)
        .execute(db.pool())
        .await
        .expect("org");
    sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
        .bind(proj.as_uuid())
        .bind(org.as_uuid())
        .bind(format!("ep-{}", proj.as_uuid().simple()))
        .execute(db.pool())
        .await
        .expect("proj");
    sqlx::query(
        "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
         VALUES ($1,$2,$3,'https://example.invalid/e.git','E')",
    )
    .bind(repo.as_uuid())
    .bind(org.as_uuid())
    .bind(proj.as_uuid())
    .execute(db.pool())
    .await
    .expect("repo");
    (org, proj, repo)
}

fn request(org: OrganizationId, proj: ProjectId, repo: RepositoryId) -> IntakeRequest {
    IntakeRequest {
        organization_id: org,
        project_id: proj,
        repository_id: repo,
        title: "Fix intermittent 500 on attachment upload".into(),
        description: "Users report failures under load.".into(),
        priority: Priority::High,
        risk: RiskLevel::Medium,
        labels: vec!["Bug".into()],
        idempotency_key: None,
    }
}

/// Recording handler standing in for analysis during this phase.
struct RecordingHandler {
    calls: Arc<Mutex<Vec<JobPayload>>>,
}

impl JobHandler for RecordingHandler {
    fn handle<'a>(
        &'a self,
        _db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            self.calls.lock().expect("lock").push(payload);
            HandlerOutcome::Completed
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn intake_creates_task_run_and_job_atomically() {
    let db = test_db().await;
    let (org, proj, repo) = seed(&db).await;
    let svc = IntakeService::new(db.clone());
    let receipt = svc.submit(&request(org, proj, repo)).await.expect("submit");

    // Run exists and belongs to the task.
    let run = db.get_run(org, receipt.run_id).await.expect("run row");
    assert_eq!(run.task_id, receipt.task_id.as_uuid());

    // Exactly one bootstrap job queued for this task.
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM jobs WHERE queue='analysis' AND payload->'payload'->'data'->>'task_id' = $1",
    )
    .bind(receipt.task_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(count.0, 1);

    // Creation event recorded with system provenance.
    let events = db
        .list_events(
            org,
            hephaestus_core::event::AggregateKind::Task,
            hephaestus_core::id::HephaestusId(receipt.task_id.as_uuid()),
            10,
        )
        .await
        .expect("events");
    assert!(events.iter().any(|e| e.provenance == "system"));
}

#[tokio::test(flavor = "multi_thread")]
async fn idempotent_intake_returns_same_identity() {
    let db = test_db().await;
    let (org, proj, repo) = seed(&db).await;
    let svc = IntakeService::new(db.clone());
    let mut req = request(org, proj, repo);
    req.idempotency_key = Some(format!("dedup-{}", org.as_uuid().simple()));

    let first = svc.submit(&req).await.expect("first");
    let second = svc.submit(&req).await.expect("second");
    assert!(!first.deduplicated);
    assert!(second.deduplicated);
    assert_eq!(first.task_id, second.task_id);
    assert_eq!(first.run_id, second.run_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_claims_and_completes_bootstrap_job() {
    let db = test_db().await;
    let (org, proj, repo) = seed(&db).await;
    let svc = IntakeService::new(db.clone());
    let receipt = svc.submit(&request(org, proj, repo)).await.expect("submit");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let registry = HandlerRegistry::new().with_analyze(Arc::new(RecordingHandler {
        calls: Arc::clone(&calls),
    }));

    let queue = "analysis".to_string();
    let config = WorkerConfig {
        id: format!("test-{}", org.as_uuid().simple()),
        queues: vec![queue],
        lease_ttl_secs: 30,
        poll_interval: std::time::Duration::from_millis(20),
        concurrency: 2,
    };
    let worker = Arc::new(Worker::new(db.clone(), registry, config));
    let shutdown = CancellationToken::new();
    let w = Arc::clone(&worker);
    let shutdown_for_task = shutdown.clone();
    let handle = tokio::spawn(async move { w.run(shutdown_for_task).await });

    // Wait until the bootstrap job was handled.
    // Workers share queues across concurrent tests; wait until THIS
    // task's bootstrap job shows up rather than any job at all.
    let want = receipt.task_id.to_string();
    let mut handled = false;
    for _ in 0..200 {
        let hit = calls.lock().expect("lock").iter().any(|p| {
            matches!(
                p, JobPayload::AnalyzeRepository { task_id, .. } if task_id.to_string() == want
            )
        });
        if hit {
            handled = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    shutdown.cancel();
    let _ = handle.await;

    assert!(handled, "bootstrap job must be processed by worker");
    let taken = calls.lock().expect("lock").clone();
    let ours = taken.iter().find(|p| {
        matches!(
            p, JobPayload::AnalyzeRepository { task_id, .. } if task_id.to_string() == want
        )
    });
    match ours {
        Some(JobPayload::AnalyzeRepository { task_id, run_id }) => {
            assert_eq!(*task_id, receipt.task_id);
            assert_eq!(*run_id, receipt.run_id);
        }
        other => panic!("our bootstrap job never handled: {other:?}"),
    }

    // Job marked done.
    let done: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM jobs WHERE status='done' AND payload->'payload'->'data'->>'task_id' = $1",
    )
    .bind(receipt.task_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("done count");
    assert_eq!(done.0, 1);
}
