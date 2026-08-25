//! End-to-end delivery pipeline against real PostgreSQL, real git and
//! real cargo (ADR-008).
//!
//! Covers the tail of the lifecycle once review parks a run at
//! `awaiting_merge`: an external merge decision advances the run,
//! bootstraps its build evidence row and chains the deterministic
//! build through the governed tool runtime. A clean change completes
//! via skip-deployment; a change that cannot build fails the run
//! loudly; a plan demanding deployment fails instead of simulating.
//! A replayed merge decision re-derives its outcome without
//! double-spending anything.

#![allow(clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use hephaestus_core::domain::{Plan, PlanStep, StrategyNotes};
use hephaestus_core::id::{OrganizationId, PlanId, ProjectId, RepositoryId, TaskId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_db::Db;
use hephaestus_db::tasks::NewTask;
use hephaestus_engine::delivery::{
    ARTIFACT_DIR, BuildHandler, MergeDecisionInput, MergeOutcome, MergeService,
};
use hephaestus_engine::worker::JobHandler;
use hephaestus_engine::{JobPayload, WorkspaceLayout};
use hephaestus_repo::git::GitRepo;

/// The scenarios share durable queues; serializing keeps assertions
/// about job rows predictable across concurrent test binaries.
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

/// Fresh tenant rows for one scenario. The remote is unreachable on
/// purpose: delivery never clones, only builds what earlier stages
/// left behind.
async fn seed_task_and_run(db: &Db) -> (OrganizationId, TaskId, WorkflowRunId) {
    let org = OrganizationId::generate();
    let proj = ProjectId::generate();
    let repo = RepositoryId::generate();
    let slug = format!("dlv-{}", org.as_uuid().simple());
    sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
        .bind(org.as_uuid())
        .bind(&slug)
        .execute(db.pool())
        .await
        .expect("org");
    sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
        .bind(proj.as_uuid())
        .bind(org.as_uuid())
        .bind(format!("dlp-{}", proj.as_uuid().simple()))
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
            title: "delivery pipeline",
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
const MAIN_RS_BROKEN: &str = "fn main( {}\n";

/// Materialize the run's repository snapshot as a real committed git
/// worktree holding a tiny cargo crate that builds cleanly.
fn make_workspace(storage_root: &Path, run: WorkflowRunId) -> GitRepo {
    let repo_dir = storage_root.join("runs").join(run.to_string()).join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).expect("mkdir");
    std::fs::write(repo_dir.join("Cargo.toml"), CARGO_TOML).expect("manifest");
    std::fs::write(repo_dir.join("src/main.rs"), MAIN_RS_CLEAN).expect("main");
    let git = GitRepo::init(&repo_dir).expect("git init");
    git.commit_all("initial", ("tester", "tester@example.invalid"))
        .expect("initial commit");
    git
}

/// Walk the legal transitions from created all the way to
/// awaiting_merge and leave behind a passed execution over a seeded
/// plan - exactly the durable state review leaves when it parks a run.
async fn seed_to_awaiting_merge(
    db: &Db,
    org: OrganizationId,
    task: TaskId,
    run: WorkflowRunId,
    deployment: Option<&str>,
) -> PlanId {
    let steps = [
        TransitionEvent::StartAnalysis,
        TransitionEvent::StartPlanning,
        TransitionEvent::SubmitForApproval,
        TransitionEvent::Approve,
        TransitionEvent::StartVerification,
        TransitionEvent::VerificationPassed,
        TransitionEvent::ReviewPassed,
    ];
    let mut state = WorkflowState::Created;
    for event in steps {
        state = db
            .transition_run(org, run, state, event, "seed")
            .await
            .expect("legal seed transition");
    }
    assert_eq!(state, WorkflowState::AwaitingMerge);

    let plan_id = PlanId::generate();
    let plan = Plan {
        id: plan_id,
        task_id: task,
        objective: "Deliver the change set.".into(),
        steps: vec![PlanStep {
            id: hephaestus_core::id::StepId::generate(),
            plan_id,
            position: 1,
            action: "Implement the change".into(),
            verification: "unit:change".into(),
            risks: vec![],
        }],
        affected_components: vec![],
        affected_symbols: vec![],
        strategy: StrategyNotes {
            rollback: Some("revert".into()),
            deployment: deployment.map(str::to_string),
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
    // The review stage opens this gate when parking the run; seeding
    // reproduces that effect directly.
    db.open_merge_gate(org, task, run)
        .await
        .expect("open merge gate");
    plan_id
}

fn merge_input() -> MergeDecisionInput {
    MergeDecisionInput {
        merged_by: "ci-merger".into(),
        reason: Some("green CI".into()),
        external_ref: Some("commit:abc1234".into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn merged_run_builds_and_completes_without_deployment() {
    let _guard = SERIAL.lock().await;
    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);
    seed_to_awaiting_merge(&db, org, task, run, None).await;

    let service = MergeService;
    let outcome = service
        .decide(&db, org, run, &merge_input())
        .await
        .expect("merge applies");
    assert_eq!(outcome, MergeOutcome::Merged);

    let scope = db.run_scope(run).await.expect("scope");
    assert_eq!(scope.state, WorkflowState::Building);

    // Replay derives its outcome from visible state: same gate, same
    // build row, one job.
    let replay = service
        .decide(&db, org, run, &merge_input())
        .await
        .expect("replay applies");
    assert_eq!(replay, MergeOutcome::AlreadyMerged);

    let build = db
        .active_build_for_run(org, run)
        .await
        .expect("build lookup")
        .expect("running build row");

    // Exactly one chained build job for THIS run despite the replay.
    let jobs: Vec<(String,)> = sqlx::query_as(
        "SELECT idempotency_key FROM jobs WHERE queue='build' AND status='pending'
          AND payload->'payload'->'data'->>'run_id'=$1",
    )
    .bind(run.as_uuid().to_string())
    .fetch_all(db.pool())
    .await
    .expect("jobs");
    assert_eq!(jobs.len(), 1, "idempotency key must dedupe the chain");
    let payload = JobPayload::BuildArtifact {
        task_id: task,
        run_id: run,
    };

    let handler = BuildHandler::new(layout.clone());
    let verdict = handler.handle(db.clone(), payload).await;
    assert_eq!(
        verdict,
        hephaestus_engine::HandlerOutcome::Completed,
        "clean change set must build"
    );

    let finished = db.run_scope(run).await.expect("scope");
    assert_eq!(finished.state, WorkflowState::Completed);

    let row = db
        .latest_build_for_run(org, run)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.id, build.id, "the same row was finished, not replaced");
    assert_eq!(row.status, "succeeded");
    let sha = row.artifact_sha256.expect("fixture produces a binary");
    assert_eq!(sha.len(), 64, "sha256 hex digest");
    assert_eq!(row.artifact_path.as_deref(), Some(ARTIFACT_DIR));

    // One typed build event with user provenance on the gate decision
    // plus computed provenance on the build outcome.
    let events: Vec<(String,)> = sqlx::query_as(
        "SELECT payload->>'type' FROM events WHERE aggregate='build' AND aggregate_id=$1",
    )
    .bind(row.id)
    .fetch_all(db.pool())
    .await
    .expect("events");
    assert_eq!(events, [(String::from("build_completed"),)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn unbuildable_change_fails_the_run_loudly() {
    let _guard = SERIAL.lock().await;
    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);
    seed_to_awaiting_merge(&db, org, task, run, None).await;

    // Break the frozen change set AFTER the merge decision: the build
    // must refuse to pretend anything shipped.
    let repo_dir = storage
        .path()
        .join("runs")
        .join(run.to_string())
        .join("repo");
    std::fs::write(repo_dir.join("src/main.rs"), MAIN_RS_BROKEN).expect("break sources");

    MergeService
        .decide(&db, org, run, &merge_input())
        .await
        .expect("merge applies");

    let handler = BuildHandler::new(layout);
    let verdict = std::pin::pin!(handler.handle(
        db.clone(),
        JobPayload::BuildArtifact {
            task_id: task,
            run_id: run
        }
    ))
    .await;
    assert_eq!(
        verdict,
        hephaestus_engine::HandlerOutcome::FailedPermanent,
        "a broken build is a permanent, loud failure"
    );

    assert_eq!(
        db.run_scope(run).await.expect("scope").state,
        WorkflowState::Failed
    );
    let row = db
        .latest_build_for_run(org, run)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.status, "failed");
    assert!(row.artifact_sha256.is_none());

    let ok: Vec<(bool,)> = sqlx::query_as(
        "SELECT (payload->'data'->>'ok')::boolean FROM events
          WHERE aggregate='build' AND aggregate_id=$1 AND payload->>'type'='build_completed'",
    )
    .bind(row.id)
    .fetch_all(db.pool())
    .await
    .expect("events");
    assert_eq!(ok, [(false,)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn deployment_demands_fail_rather_than_simulate() {
    let _guard = SERIAL.lock().await;
    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);
    seed_to_awaiting_merge(&db, org, task, run, Some("ship via internal CD")).await;

    MergeService
        .decide(&db, org, run, &merge_input())
        .await
        .expect("merge applies");

    let handler = BuildHandler::new(layout);
    let verdict = std::pin::pin!(handler.handle(
        db.clone(),
        JobPayload::BuildArtifact {
            task_id: task,
            run_id: run
        }
    ))
    .await;
    assert_eq!(
        verdict,
        hephaestus_engine::HandlerOutcome::FailedPermanent,
        "no deployment executor exists, so completion would be simulation"
    );

    // The build itself genuinely succeeded; the RUN failed because the
    // demanded next hop is impossible. Both facts stay honest in
    // evidence.
    assert_eq!(
        db.run_scope(run).await.expect("scope").state,
        WorkflowState::Failed
    );
    let row = db
        .latest_build_for_run(org, run)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.status, "succeeded");
    assert!(row.artifact_sha256.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn merges_are_refused_outside_awaiting_merge() {
    let _guard = SERIAL.lock().await;
    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, _task, run) = seed_task_and_run(&db).await;
    make_workspace(storage.path(), run);

    // Still in created: no gate, no merge.
    let err = MergeService
        .decide(&db, org, run, &merge_input())
        .await
        .expect_err("early merge refused");
    assert!(matches!(err, hephaestus_core::Error::Conflict { .. }));
}
