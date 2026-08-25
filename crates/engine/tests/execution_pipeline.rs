//! End-to-end execution pipeline against real PostgreSQL and real git.
//!
//! Covers the back half of the lifecycle from an approved plan onward:
//! approval decision bootstraps an execution -> governed implementation
//! of plan steps (a scripted model driving REAL fs tools inside a REAL
//! git worktree) -> deterministic verification layers (REAL cargo) ->
//! automated review verdict -> `awaiting_merge`. A second scenario
//! proves the review change-loop budget fails a run that never makes
//! progress instead of ping-ponging forever.

#![allow(clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use hephaestus_agent::provider::{CompletionRequest, CompletionResponse, ModelProvider};
use hephaestus_agent::session::{CollectingSink, DecisionSink};
use hephaestus_core::domain::{Plan, PlanStep, StrategyNotes};
use hephaestus_core::id::{OrganizationId, PlanId, ProjectId, RepositoryId, TaskId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_db::Db;
use hephaestus_db::tasks::NewTask;
use hephaestus_engine::approval::{ApprovalDecisionInput, ApprovalOutcome, ApprovalService};
use hephaestus_engine::execution::{ExecutionHandler, RepairHandler};
use hephaestus_engine::review::{MAX_REVIEW_ROUNDS, ReviewHandler};
use hephaestus_engine::verification::RunVerificationHandler;
use hephaestus_engine::worker::{HandlerRegistry, Worker, WorkerConfig};
use hephaestus_engine::{SessionDeps, WorkspaceLayout};
use hephaestus_repo::git::GitRepo;
use hephaestus_tools::invocation::AuthzDecision;
use tokio_util::sync::CancellationToken;

/// The two scenarios share the durable job queues with each other and
/// with any other test process against this database; serializing them
/// keeps scripted responses predictable.
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

/// Fresh tenant rows for one scenario. The handlers under test never
/// resolve the remote themselves, but the tasks table requires one -
/// it points somewhere unreachable on purpose.
async fn seed_task_and_run(db: &Db) -> (OrganizationId, TaskId, WorkflowRunId) {
    let org = OrganizationId::generate();
    let proj = ProjectId::generate();
    let repo = RepositoryId::generate();
    let slug = format!("exec-{}", org.as_uuid().simple());
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
    let task = db
        .create_task(&NewTask {
            organization_id: org,
            project_id: proj,
            repository_id: repo,
            title: "execution pipeline",
            description: "e2e",
            priority: "high",
            risk: "medium",
            labels: &[],
            idempotency_key: None,
        })
        .await
        .expect("task");
    let run = db.create_run(org, task).await.expect("run");
    (org, task, run)
}

const CARGO_TOML: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const MAIN_RS_CLEAN: &str = "fn main() {}\n";
const MAIN_RS_IMPLEMENTED: &str =
    "/// Entry point touched by the implementer.\nfn main() {\n    println!(\"hello\");\n}\n";
const README_BEFORE: &str = "# fixture\n";
const README_AFTER: &str = "# fixture\n\nImplemented by the governed implementer.\n";

/// Materialize the run's repository snapshot as a real committed git
/// worktree holding a tiny fmt-clean cargo crate. Direct seeding skips
/// the analysis stage, so the workspace must exist before any
/// implementation or verification runs inside it.
fn make_workspace(storage_root: &Path, run: WorkflowRunId) {
    let repo_dir = storage_root.join("runs").join(run.to_string()).join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).expect("mkdir");
    std::fs::write(repo_dir.join("Cargo.toml"), CARGO_TOML).expect("manifest");
    std::fs::write(repo_dir.join("src/main.rs"), MAIN_RS_CLEAN).expect("main");
    std::fs::write(repo_dir.join("README.md"), README_BEFORE).expect("readme");
    let git = GitRepo::init(&repo_dir).expect("git init");
    git.commit_all("initial", ("tester", "tester@example.invalid"))
        .expect("initial commit");
}

/// Seed a two-step plan over the task, open its plan-approval gate and
/// walk the state machine to `awaiting_approval` - exactly where the
/// planning pipeline parks a run once a plan exists.
async fn seed_plan_and_gate(
    db: &Db,
    org: OrganizationId,
    task: TaskId,
    run: WorkflowRunId,
    verification_layers: &[&str],
) -> PlanId {
    db.transition_run(
        org,
        run,
        WorkflowState::Created,
        TransitionEvent::StartAnalysis,
        "seed",
    )
    .await
    .expect("to analyzing");
    db.transition_run(
        org,
        run,
        WorkflowState::Analyzing,
        TransitionEvent::StartPlanning,
        "seed",
    )
    .await
    .expect("to planning");

    let plan_id = PlanId::generate();
    let plan = Plan {
        id: plan_id,
        task_id: task,
        objective: "Make the fixture greet and document it.".into(),
        steps: vec![
            PlanStep {
                id: hephaestus_core::id::StepId::generate(),
                plan_id,
                position: 1,
                action: "Implement the greeting in src/main.rs".into(),
                verification: "unit:greeting_builds".into(),
                risks: vec![],
            },
            PlanStep {
                id: hephaestus_core::id::StepId::generate(),
                plan_id,
                position: 2,
                action: "Document the change in README.md".into(),
                verification: "unit:docs_updated".into(),
                risks: vec![],
            },
        ],
        affected_components: vec![],
        affected_symbols: vec![],
        strategy: StrategyNotes {
            rollback: Some("revert the commit".into()),
            deployment: None,
            verification: verification_layers
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        },
        created_at: chrono::Utc::now(),
        prompt_version: Some("test-v1".into()),
    };
    db.create_plan(org, task, run, &plan)
        .await
        .expect("create plan");
    db.open_plan_approval(org, task, run)
        .await
        .expect("open gate");
    db.transition_run(
        org,
        run,
        WorkflowState::Planning,
        TransitionEvent::SubmitForApproval,
        "seed",
    )
    .await
    .expect("to awaiting_approval");
    plan_id
}

/// Delete pending backlog jobs whose driving run has been idle for
/// over 90 seconds: leftovers from processes that already exited would
/// otherwise be claimed ahead of this test's jobs and consume its
/// scripted provider output.
async fn reap_stale_jobs(db: &Db) {
    sqlx::query(
        "DELETE FROM jobs WHERE status='pending'
         AND queue IN ('analysis','planning','implementation','verification','review')
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

/// Pad one scripted response so stray redeliveries consume copies
/// instead of starving this run's chain.
fn padded(response: &str, n: usize) -> Vec<String> {
    std::iter::repeat_n(response.to_string(), n).collect()
}

/// Wrap a stage document as the model's final answer under the session
/// protocol (the runtime unwraps `final`; handlers re-parse the inner
/// document deterministically).
fn final_of(doc: &str) -> String {
    serde_json::json!({ "final": doc }).to_string()
}

/// One fs.write tool call in session-protocol form.
fn write_tool(path: &str, content: &str) -> String {
    serde_json::json!({
        "thought": "edit tracked file",
        "tool": {"name": "fs.write", "args": {"path": path, "content": content}}
    })
    .to_string()
}

fn step_completed_doc(summary: &str, files: &[&str]) -> String {
    final_of(
        &serde_json::json!({
            "status": "completed",
            "summary": summary,
            "files_changed": files
        })
        .to_string(),
    )
}

fn review_approve_doc() -> String {
    final_of(&serde_json::json!({"decision": "approve", "notes": "clean change"}).to_string())
}

/// Worker subscribed exactly to the execution-phase queues. Analysis
/// and planning are deliberately NOT served here so concurrent
/// front-half tests can never steal this provider's output.
fn execution_worker(
    db: &Db,
    deps: SessionDeps,
    layout: WorkspaceLayout,
    worker_id: String,
) -> (Arc<Worker>, CancellationToken) {
    let registry = HandlerRegistry::new()
        .with_execute_step(Arc::new(ExecutionHandler::new(
            deps.clone(),
            layout.clone(),
        )))
        .with_repair_execution(Arc::new(RepairHandler::new(deps.clone(), layout.clone())))
        .with_run_verification(Arc::new(RunVerificationHandler::new(layout.clone())))
        .with_run_review(Arc::new(ReviewHandler::new(deps, layout)));
    let config = WorkerConfig {
        id: worker_id,
        queues: vec![
            "implementation".to_string(),
            "verification".to_string(),
            "review".to_string(),
        ],
        lease_ttl_secs: 30,
        poll_interval: std::time::Duration::from_millis(20),
        concurrency: 2,
    };
    let shutdown = CancellationToken::new();
    (
        Arc::new(Worker::new(db.clone(), registry, config)),
        shutdown,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn approved_plan_executes_verifies_and_reviews_to_awaiting_merge() {
    let _guard = SERIAL.lock().await;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);

    // Session-exact script. Unlike the single-response planning
    // stages, implementation sessions span MULTIPLE provider turns
    // (tool call, then final), so blanket padding corrupts the
    // sequence: a session opening on the previous stage's leftover
    // response either skips its work or proposes tools outside its
    // role. These queues serve no workers but this binary's, and jobs
    // complete well inside their leases, so the exact chain below is
    // deterministic; one trailing spare absorbs a single redelivery.
    let responses = [
        write_tool("src/main.rs", MAIN_RS_IMPLEMENTED),
        step_completed_doc("greeting implemented in main", &["src/main.rs"]),
        write_tool("README.md", README_AFTER),
        step_completed_doc("README documents the greeting", &["README.md"]),
        review_approve_doc(),
        review_approve_doc(),
    ]
    .map(String::from)
    .to_vec();

    let scripted = Arc::new(Scripted::new(responses));
    let sink = Arc::new(CollectingSink::default());
    let deps = SessionDeps::new(
        Arc::clone(&scripted) as Arc<dyn ModelProvider>,
        Arc::clone(&sink) as Arc<dyn DecisionSink>,
        "test-model",
    );

    reap_stale_jobs(&db).await;
    let plan_id = seed_plan_and_gate(&db, org, task, run, &["format"]).await;

    // The human decision at the gate: approve and bootstrap execution.
    let service = ApprovalService::new(deps.clone(), layout.clone());
    let outcome = service
        .decide(
            &db,
            org,
            run,
            &ApprovalDecisionInput {
                approver: "tester".into(),
                approved: true,
                reason: Some("plan covers the requirement".into()),
            },
        )
        .await
        .expect("decision applies");
    let ApprovalOutcome::Approved { execution_id, .. } = outcome else {
        panic!("approval must bootstrap an execution");
    };

    let (worker, shutdown) = execution_worker(
        &db,
        deps,
        layout,
        format!("exec-{}", org.as_uuid().simple()),
    );
    let w = Arc::clone(&worker);
    let token = shutdown.clone();
    let handle = tokio::spawn(async move { w.run(token).await });

    let reached = wait_for_state(&db, run, WorkflowState::AwaitingMerge, 120).await;
    shutdown.cancel();
    let _ = handle.await;
    assert!(
        reached,
        "pipeline must reach awaiting_merge; stuck at {:?}",
        db.run_scope(run).await.map(|s| s.state)
    );

    // Execution closed as passed over the seeded plan, every step
    // completed in order with a non-empty summary.
    let exec = db
        .get_execution(org, execution_id)
        .await
        .expect("execution");
    assert_eq!(exec.status, "passed");
    assert_eq!(exec.plan_id, plan_id.as_uuid());
    let steps = db
        .list_execution_steps(org, execution_id)
        .await
        .expect("steps");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps.iter().map(|s| s.position).collect::<Vec<_>>(), [1, 2]);
    assert!(steps.iter().all(|s| s.status == "completed"));
    assert!(
        steps[0].summary.contains("greeting") && steps[1].summary.contains("README"),
        "each step must file its own outcome, got {steps:?}"
    );

    // Exactly one verification suite ran and passed.
    let latest = db
        .latest_verification(org, execution_id)
        .await
        .expect("verifications")
        .expect("suite recorded");
    assert_eq!(latest.cycle, 1);
    assert!(latest.passed);

    // The reviewer's approve verdict is durable evidence.
    let verdicts: Vec<String> = sqlx::query_scalar(
        "SELECT payload->'data'->>'decision' FROM events
          WHERE aggregate='execution' AND aggregate_id=$1
            AND payload->>'type'='review_completed'",
    )
    .bind(execution_id.as_uuid())
    .fetch_all(db.pool())
    .await
    .expect("events");
    assert_eq!(verdicts, ["approved"]);

    // Implementation really happened on disk through the governed
    // tools, exactly once per step and in the right files.
    let repo_dir = storage
        .path()
        .join("runs")
        .join(run.to_string())
        .join("repo");
    assert_eq!(
        std::fs::read_to_string(repo_dir.join("src/main.rs")).expect("implemented source"),
        MAIN_RS_IMPLEMENTED
    );
    assert_eq!(
        std::fs::read_to_string(repo_dir.join("README.md")).expect("documented readme"),
        README_AFTER
    );

    // Every write was authorized and audited under the role actor; the
    // reviewer needed no tool calls for its scripted verdict.
    let decisions = sink.snapshot();
    let writes = decisions
        .iter()
        .filter(|d| d.tool == "fs.write" && d.actor == "agent:implementer")
        .count();
    assert_eq!(writes, 2, "both steps must write through audited tools");
    assert!(
        decisions
            .iter()
            .all(|d| d.decision == AuthzDecision::Granted),
        "no denial is expected in the happy path"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn review_change_loops_without_progress_fail_the_run() {
    let _guard = SERIAL.lock().await;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);

    // The implementer claims completion without ever touching a
    // tracked file: verification keeps passing while review keeps
    // finding an empty diff - the deterministic request-changes loop.
    // No reviewer responses are needed because an empty diff
    // short-circuits before any model call. Two repairs consume the
    // budget; the third request-changes verdict exceeds
    // MAX_REVIEW_ROUNDS and fails the run.
    let mut responses = Vec::new();
    responses.extend(padded(
        &step_completed_doc("no changes were needed", &[]),
        12,
    ));

    let scripted = Arc::new(Scripted::new(responses));
    let deps = SessionDeps::new(scripted, Arc::new(CollectingSink::default()), "test-model");

    reap_stale_jobs(&db).await;
    seed_plan_and_gate(&db, org, task, run, &["format"]).await;

    let service = ApprovalService::new(deps.clone(), layout.clone());
    let outcome = service
        .decide(
            &db,
            org,
            run,
            &ApprovalDecisionInput {
                approver: "tester".into(),
                approved: true,
                reason: None,
            },
        )
        .await
        .expect("decision applies");
    let execution_id = match outcome {
        ApprovalOutcome::Approved { execution_id, .. } => execution_id,
        ApprovalOutcome::Rejected { .. } => panic!("approval was requested"),
    };

    let (worker, shutdown) = execution_worker(
        &db,
        deps,
        layout,
        format!("exec-loop-{}", org.as_uuid().simple()),
    );
    let w = Arc::clone(&worker);
    let token = shutdown.clone();
    let handle = tokio::spawn(async move { w.run(token).await });

    let reached = wait_for_state(&db, run, WorkflowState::Failed, 120).await;
    shutdown.cancel();
    let _ = handle.await;
    assert!(
        reached,
        "change-loop budget must fail the run; stuck at {:?}",
        db.run_scope(run).await.map(|s| s.state)
    );

    // Terminal evidence: failed execution, zero progress, budget rows.
    let exec = db
        .get_execution(org, execution_id)
        .await
        .expect("execution");
    assert_eq!(exec.status, "failed");

    let failed_suites = db
        .count_failed_verifications(org, execution_id)
        .await
        .expect("count");
    assert_eq!(
        failed_suites, 0,
        "format kept passing; the failure is review-owned"
    );

    let total_suites: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM verifications WHERE execution_id=$1")
            .bind(execution_id.as_uuid())
            .fetch_one(db.pool())
            .await
            .expect("suite count");
    assert_eq!(total_suites, MAX_REVIEW_ROUNDS + 1);

    let findings: Vec<String> = sqlx::query_scalar(
        "SELECT payload->'data'->'blocking'->>0 FROM events
          WHERE aggregate='execution' AND aggregate_id=$1
            AND payload->>'type'='review_completed'",
    )
    .bind(execution_id.as_uuid())
    .fetch_all(db.pool())
    .await
    .expect("events");
    assert_eq!(
        findings.len() as i64,
        MAX_REVIEW_ROUNDS + 1,
        "every empty-diff round must record its blocking finding"
    );
    assert!(
        findings.iter().all(|f| f.contains("no changes")),
        "findings must come from the deterministic empty-diff path: {findings:?}"
    );
}
