//! End-to-end planning pipeline against real PostgreSQL and real git.
//!
//! Covers the full front half of the lifecycle:
//! intake -> analyze (clone + inventory) -> extract requirements
//! (governed planner session) -> generate plan (governed planner
//! session) -> approval gate. Handler-level failure classification is
//! covered by direct-call tests that skip the worker loop.

#![allow(clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use hephaestus_agent::provider::{CompletionRequest, CompletionResponse, ModelProvider};
use hephaestus_agent::session::CollectingSink;
use hephaestus_core::domain::{Priority, RiskLevel};
use hephaestus_core::id::{OrganizationId, ProjectId, RepositoryId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_db::Db;
use hephaestus_db::tasks::NewTask;
use hephaestus_engine::SessionDeps;
use hephaestus_engine::analysis::{AnalysisHandler, WorkspaceLayout};
use hephaestus_engine::intake::{IntakeRequest, IntakeService};
use hephaestus_engine::jobs::JobPayload;
use hephaestus_engine::planning::{ExtractionHandler, PlanningHandler};
use hephaestus_engine::worker::{
    HandlerOutcome, HandlerRegistry, JobHandler, Worker, WorkerConfig,
};
use hephaestus_repo::git::GitRepo;
use hephaestus_tools::invocation::AuthzDecision;
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

/// Seed tenant rows whose repository points at a LOCAL git path so the
/// analysis stage can genuinely clone it.
async fn seed_with_local_repo(db: &Db, remote: &str) -> (OrganizationId, ProjectId, RepositoryId) {
    let org = OrganizationId::generate();
    let proj = ProjectId::generate();
    let repo = RepositoryId::generate();
    let slug = format!("pipe-{}", org.as_uuid().simple());
    sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
        .bind(org.as_uuid())
        .bind(&slug)
        .execute(db.pool())
        .await
        .expect("org");
    sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
        .bind(proj.as_uuid())
        .bind(org.as_uuid())
        .bind(format!("pp-{}", proj.as_uuid().simple()))
        .execute(db.pool())
        .await
        .expect("proj");
    sqlx::query(
        "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
         VALUES ($1,$2,$3,$4,'E')",
    )
    .bind(repo.as_uuid())
    .bind(org.as_uuid())
    .bind(proj.as_uuid())
    .bind(remote)
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

/// Provider replaying a fixed script of responses.
struct Scripted {
    queue: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(responses: Vec<String>) -> Self {
        Self {
            queue: Mutex::new(responses),
        }
    }
}

impl ModelProvider for Scripted {
    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = hephaestus_core::Result<CompletionResponse>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut q = self
                .queue
                .lock()
                .map_err(|_| hephaestus_core::Error::Config("poisoned script".into()))?;
            if q.is_empty() {
                return Err(hephaestus_core::Error::Config("script exhausted".into()));
            }
            let text = q.remove(0);
            drop(q);
            Ok(CompletionResponse {
                text,
                model: req.model,
                prompt_tokens: None,
                completion_tokens: None,
            })
        })
    }

    fn name(&self) -> &str {
        "scripted"
    }
}

/// Delete pending backlog jobs whose driving run has been idle for
/// over 90 seconds.
///
/// Workers share queues across test processes; leftovers from earlier
/// runs would otherwise be claimed (oldest `run_after` first) ahead of
/// this test's jobs and consume its scripted provider output. Runs
/// idle that long belong to processes that already exited.
async fn reap_stale_jobs(db: &Db) {
    sqlx::query(
        "DELETE FROM jobs WHERE status='pending'
         AND queue IN ('analysis','planning')
         AND payload->'payload'->'data'->>'run_id' IN (
             SELECT id::text FROM workflow_runs
              WHERE last_transition_at < now() - interval '90 seconds')",
    )
    .execute(db.pool())
    .await
    .expect("reap stale jobs");
}

async fn wait_for_state(db: &Db, run: WorkflowRunId, want: WorkflowState, secs: u64) -> bool {
    for _ in 0..secs * 40 {
        if let Ok(scope) = db.run_scope(run).await
            && scope.state == want
        {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    false
}

const REQUIREMENTS_DOC: &str = r#"{"requirements":[{"category":"functional","kind":"explicit","statement":"Upload succeeds below the configured size limit.","source":"task-description"},{"category":"performance","kind":"assumption","statement":"Production load profile is unknown.","source":"needs load report"}]}"#;

/// Wrap a document as the model's final answer under the session
/// protocol (the runtime unwraps `final`; handlers re-parse the inner
/// document deterministically).
fn final_of(doc: &str) -> String {
    serde_json::json!({ "final": doc }).to_string()
}

const PLAN_DOC: &str = r#"{"objective":"Fix intermittent 500 on attachment upload.","steps":[{"action":"Add size guard in upload handler src/upload.rs","verification":"unit:upload_size_guard","risks":["behavior change"]},{"action":"Add regression test upload_rejects_oversized","verification":"unit:regression","risks":[]}],"affected_components":["upload"],"affected_symbols":[],"strategy":{"rollback":"revert commit","deployment":null,"verification":["unit"]}}"#;

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_reaches_approval_gate_end_to_end() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    // A real local git repository as clone source.
    let origin = tempfile::tempdir().expect("origin dir");
    let git = GitRepo::init(origin.path()).expect("init");
    std::fs::write(origin.path().join("README.md"), "# Sample\nupload docs\n").expect("seed file");
    git.commit_all("initial", ("tester", "tester@example.invalid"))
        .expect("commit");

    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, proj, repo_id) =
        seed_with_local_repo(&db, origin.path().to_str().expect("utf8")).await;
    let receipt = IntakeService::new(db.clone())
        .submit(&request(org, proj, repo_id))
        .await
        .expect("intake");

    // Final answers travel inside the session's strict JSON envelope:
    // {"final": "<document>"}. The document itself is the string value.
    // Each stage's answer is padded: a stray claim on a leftover job
    // consumes one copy instead of starving this run's chain.
    let mut responses = Vec::new();
    responses.extend(std::iter::repeat_n(final_of(REQUIREMENTS_DOC), 8));
    responses.extend(std::iter::repeat_n(final_of(PLAN_DOC), 8));
    let scripted = Arc::new(Scripted::new(responses));
    let deps = SessionDeps::new(scripted, Arc::new(CollectingSink::default()), "test-model");
    let layout = WorkspaceLayout::new(storage.path());

    let registry = HandlerRegistry::new()
        .with_analyze(Arc::new(AnalysisHandler::new(layout.clone())))
        .with_extract_requirements(Arc::new(ExtractionHandler::new(
            deps.clone(),
            layout.clone(),
        )))
        .with_generate_plan(Arc::new(PlanningHandler::new(deps, layout)));

    // Clear leftover backlog BEFORE our jobs exist so nothing can
    // consume the scripted provider output out from under this run.
    reap_stale_jobs(&db).await;

    let config = WorkerConfig {
        id: format!("pipe-{}", org.as_uuid().simple()),
        queues: vec!["analysis".to_string(), "planning".to_string()],
        lease_ttl_secs: 30,
        poll_interval: std::time::Duration::from_millis(20),
        concurrency: 2,
    };
    let worker = Arc::new(Worker::new(db.clone(), registry, config));
    let shutdown = CancellationToken::new();
    let w = Arc::clone(&worker);
    let shutdown_for_task = shutdown.clone();
    let handle = tokio::spawn(async move { w.run(shutdown_for_task).await });

    let reached = wait_for_state(&db, receipt.run_id, WorkflowState::AwaitingApproval, 25).await;
    shutdown.cancel();
    let _ = handle.await;
    assert!(reached, "pipeline must reach awaiting_approval");

    // Requirements persisted with correct epistemics.
    let reqs = db
        .list_requirements(org, receipt.task_id)
        .await
        .expect("reqs");
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].is_gating());
    assert!(!reqs[1].is_gating(), "assumptions must not gate");

    // Plan persisted with ordered steps and prompt version.
    let plan = db
        .get_current_plan(org, receipt.task_id)
        .await
        .expect("query")
        .expect("current plan");
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(
        plan.steps.iter().map(|s| s.position).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(plan.prompt_version.as_deref(), Some("planner-v1"));

    // Human approval gate open.
    let gate = db
        .get_plan_approval(org, receipt.run_id)
        .await
        .expect("gate row")
        .expect("open gate");
    assert_eq!(gate.gate, "plan");
    assert_eq!(gate.decision, None);

    // Snapshot really cloned from the local origin.
    let snapshot = storage
        .path()
        .join("runs")
        .join(receipt.run_id.to_string())
        .join("repo")
        .join("README.md");
    assert!(snapshot.exists(), "snapshot README must exist");

    // Deterministic analysis event recorded.
    let types: Vec<String> = sqlx::query_scalar(
        "SELECT payload->>'type' FROM events WHERE aggregate='task' AND aggregate_id=$1",
    )
    .bind(receipt.task_id.as_uuid())
    .fetch_all(db.pool())
    .await
    .expect("events");
    assert!(types.iter().any(|t| t == "repository_analyzed"));
    assert!(types.iter().any(|t| t == "requirements_extracted"));
    assert!(types.iter().any(|t| t == "plan_generated"));

    // Queue accounting scoped to this run: workers share queues across
    // concurrent tests, so never count the whole table.
    let run_str = receipt.run_id.to_string();
    let analysis_done: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM jobs WHERE queue='analysis' AND status='done'
         AND payload->'payload'->'data'->>'run_id' = $1",
    )
    .bind(&run_str)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(analysis_done.0, 2);
    let planning_done: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM jobs WHERE queue='planning' AND status='done'
         AND payload->'payload'->'data'->>'run_id' = $1",
    )
    .bind(&run_str)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(planning_done.0, 1);
}

/// Fresh task+run pair without going through intake (handlers under
/// test do not need a repository row).
async fn seeded_task_and_run(
    db: &Db,
) -> (OrganizationId, hephaestus_core::id::TaskId, WorkflowRunId) {
    let org = OrganizationId::generate();
    let proj = ProjectId::generate();
    let repo = RepositoryId::generate();
    let slug = format!("dir-{}", org.as_uuid().simple());
    sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
        .bind(org.as_uuid())
        .bind(&slug)
        .execute(db.pool())
        .await
        .expect("org");
    sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
        .bind(proj.as_uuid())
        .bind(org.as_uuid())
        .bind(format!("dp-{}", proj.as_uuid().simple()))
        .execute(db.pool())
        .await
        .expect("proj");
    sqlx::query(
        "INSERT INTO repositories (id, organization_id, project_id, remote_url, display_name)
         VALUES ($1,$2,$3,'https://example.invalid/d.git','D')",
    )
    .bind(repo.as_uuid())
    .bind(org.as_uuid())
    .bind(proj.as_uuid())
    .execute(db.pool())
    .await
    .expect("repo");
    let task = db
        .create_task(&NewTask {
            organization_id: org,
            project_id: proj,
            repository_id: repo,
            title: "handler-level",
            description: "d",
            priority: "low",
            risk: "low",
            labels: &[],
            idempotency_key: None,
        })
        .await
        .expect("task");
    let run = db.create_run(org, task).await.expect("run");
    (org, task, run)
}

#[tokio::test(flavor = "multi_thread")]
async fn planning_without_requirements_fails_permanently() {
    let db = test_db().await;
    let (org, task, run) = seeded_task_and_run(&db).await;
    let scripted = Arc::new(Scripted::new(vec![]));
    let deps = SessionDeps::new(scripted, Arc::new(CollectingSink::default()), "m");
    let layout = WorkspaceLayout::new(tempfile::tempdir().expect("tmp").path());
    let handler = PlanningHandler::new(deps, layout);

    let outcome = handler
        .handle(
            db.clone(),
            JobPayload::GeneratePlan {
                task_id: task,
                run_id: run,
            },
        )
        .await;
    assert_eq!(outcome, HandlerOutcome::FailedPermanent);
    assert_eq!(
        db.run_scope(run).await.expect("scope").state,
        WorkflowState::Created
    );
    let _ = org;
}

#[tokio::test(flavor = "multi_thread")]
async fn garbage_planner_output_is_retryable_and_persists_nothing() {
    let db = test_db().await;
    let (org, task, run) = seeded_task_and_run(&db).await;
    db.transition_run(
        org,
        run,
        WorkflowState::Created,
        TransitionEvent::StartAnalysis,
        "t",
    )
    .await
    .expect("advance");
    db.add_requirement(
        task,
        "functional",
        "explicit",
        "Upload works below limit",
        "test",
        true,
    )
    .await
    .expect("requirement");

    // Session-protocol-valid final whose CONTENT is not a valid plan
    // document: the handler must classify this as retryable noise.
    let scripted = Arc::new(Scripted::new(vec![final_of("definitely not json")]));
    let deps = SessionDeps::new(scripted, Arc::new(CollectingSink::default()), "m");
    let layout = WorkspaceLayout::new(tempfile::tempdir().expect("tmp").path());
    let handler = PlanningHandler::new(deps, layout);

    let outcome = handler
        .handle(
            db.clone(),
            JobPayload::GeneratePlan {
                task_id: task,
                run_id: run,
            },
        )
        .await;
    assert_eq!(outcome, HandlerOutcome::Retryable);
    assert!(
        db.get_current_plan(org, task)
            .await
            .expect("none")
            .is_none()
    );
    // The stage transition happens before generation; a retry resumes
    // there instead of repeating it.
    assert_eq!(
        db.run_scope(run).await.expect("scope").state,
        WorkflowState::Planning
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn extraction_uses_tools_audited_and_chains_plan_job() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let db = test_db().await;
    let (_org, task, run) = seeded_task_and_run(&db).await;

    // Pre-seed a workspace file so the planner's fs.read is granted and
    // returns real bytes.
    let storage = tempfile::tempdir().expect("storage");
    let repo_dir = storage
        .path()
        .join("runs")
        .join(run.to_string())
        .join("repo");
    std::fs::create_dir_all(&repo_dir).expect("mkdir");
    std::fs::write(repo_dir.join("notes.txt"), "upload fails above 10mb").expect("notes");

    let scripted = Arc::new(Scripted::new(vec![
        r#"{"thought":"peek","tool":{"name":"fs.read","args":{"path":"notes.txt"}}}"#.to_string(),
        final_of(REQUIREMENTS_DOC),
    ]));
    let sink = Arc::new(CollectingSink::default());
    let deps = SessionDeps::new(
        scripted,
        Arc::clone(&sink) as Arc<dyn hephaestus_agent::session::DecisionSink>,
        "m",
    );
    let layout = WorkspaceLayout::new(storage.path());
    let handler = ExtractionHandler::new(deps, layout);

    let outcome = handler
        .handle(
            db.clone(),
            JobPayload::ExtractRequirements {
                task_id: task,
                run_id: run,
            },
        )
        .await;
    assert_eq!(outcome, HandlerOutcome::Completed);

    // Run advanced created -> analyzing.
    assert_eq!(
        db.run_scope(run).await.expect("scope").state,
        WorkflowState::Analyzing
    );

    // Requirements persisted.
    let reqs = db.list_requirements(_org, task).await.expect("reqs");
    assert_eq!(reqs.len(), 2);

    // The tool call was authorized and audited under the planner actor.
    let decisions = sink.snapshot();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].decision, AuthzDecision::Granted);
    assert_eq!(decisions[0].tool, "fs.read");
    assert_eq!(decisions[0].actor, "agent:planner");

    // Exactly one chained plan job with the deterministic key.
    let key = format!("generate-plan:{run}");
    let chained: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM jobs WHERE queue='planning' AND idempotency_key=$1 AND status='pending'",
    )
    .bind(&key)
    .fetch_one(db.pool())
    .await
    .expect("chained");
    assert_eq!(chained.0, 1);
}
