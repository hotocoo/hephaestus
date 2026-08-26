//! End-to-end deployment pipeline against real PostgreSQL, real git
//! and real cargo (ADR-013).
//!
//! Covers the last hop of the lifecycle: a plan naming a configured
//! target ships after the merge-build chain, the mandatory hooks
//! verify it, and the run completes. An unmatched target name fails
//! the run before any command runs; a failing deploy or a failing hook
//! records failed evidence and fails the run terminally; stale
//! redelivery after completion is absorbed.

#![allow(clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use hephaestus_core::domain::{Plan, PlanStep, StrategyNotes};
use hephaestus_core::id::{OrganizationId, PlanId, ProjectId, RepositoryId, TaskId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_db::Db;
use hephaestus_db::tasks::NewTask;
use hephaestus_engine::delivery::{BuildHandler, MergeDecisionInput, MergeService};
use hephaestus_engine::deployment::{DeployCommand, DeploymentSettings, DeploymentTargetSpec};
use hephaestus_engine::worker::JobHandler;
use hephaestus_engine::{DeploymentHandler, JobPayload, WorkspaceLayout};
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

async fn seed_task_and_run(db: &Db) -> (OrganizationId, TaskId, WorkflowRunId) {
    let org = OrganizationId::generate();
    let proj = ProjectId::generate();
    let repo = RepositoryId::generate();
    let slug = format!("dpl-{}", org.as_uuid().simple());
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
            title: "deployment pipeline",
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

/// Walk the legal transitions to awaiting_merge and leave behind a
/// passed execution over a seeded plan whose strategy names
/// `deployment` - the durable state review parks runs in.
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
    let mut plan = Plan {
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
            verification: vec![],
        },
        created_at: chrono::Utc::now(),
        prompt_version: Some("test-v1".into()),
    };
    plan.steps[0].plan_id = plan.id;
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
    plan_id
}

fn merge_input() -> MergeDecisionInput {
    MergeDecisionInput {
        merged_by: "ci-merger".into(),
        reason: None,
        external_ref: None,
    }
}

/// A staging target whose commands are coreutils' true/false: exit 0
/// or exit 1, nothing else, no network, fully deterministic.
fn staging_settings(deploy: &str, hooks: &[&str]) -> DeploymentSettings {
    DeploymentSettings::new(vec![DeploymentTargetSpec {
        name: "staging".into(),
        command: DeployCommand::from_argv(&[deploy.to_string()]).expect("argv"),
        verify: hooks
            .iter()
            .map(|h| DeployCommand::from_argv(&[(*h).to_string()]).expect("argv"))
            .collect(),
        timeout_secs: 60,
    }])
}

/// Merge, run the build stage with configured targets, and report the
/// raw outcome plus the resulting durable state.
async fn merge_and_build(
    db: &Db,
    layout: &WorkspaceLayout,
    settings: Arc<DeploymentSettings>,
    org: OrganizationId,
    task: TaskId,
    run: WorkflowRunId,
) -> (hephaestus_engine::HandlerOutcome, WorkflowState) {
    MergeService
        .decide(db, org, run, &merge_input())
        .await
        .expect("merge applies");

    let handler = BuildHandler::new(layout.clone()).with_deployments(Arc::clone(&settings));
    let verdict = handler
        .handle(
            db.clone(),
            JobPayload::BuildArtifact {
                task_id: task,
                run_id: run,
            },
        )
        .await;
    let state = db.run_scope(run).await.expect("scope").state;
    (verdict, state)
}

#[tokio::test(flavor = "multi_thread")]
async fn demanded_deployment_ships_verifies_and_completes() {
    let _guard = SERIAL.lock().await;
    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);
    seed_to_awaiting_merge(&db, org, task, run, Some("staging")).await;

    let settings = Arc::new(staging_settings("true", &["true", "true"]));
    let (verdict, state) =
        merge_and_build(&db, &layout, Arc::clone(&settings), org, task, run).await;
    assert_eq!(
        verdict,
        hephaestus_engine::HandlerOutcome::Completed,
        "clean build must route to deployment"
    );
    assert_eq!(state, WorkflowState::Deploying);

    // The build bootstrapped the deployment row and chained one job.
    let active = db
        .active_deployment_for_run(org, run)
        .await
        .expect("read")
        .expect("running deployment row");
    assert_eq!(active.target, "staging");
    let jobs: Vec<(String,)> = sqlx::query_as(
        "SELECT idempotency_key FROM jobs WHERE queue='deployment'
          AND idempotency_key = $1 AND status='pending'",
    )
    .bind(format!("deploy:{run}"))
    .fetch_all(db.pool())
    .await
    .expect("jobs");
    assert_eq!(jobs.len(), 1, "the deployment job must be chained once");

    let handler = DeploymentHandler::new(layout.clone(), Arc::clone(&settings));
    let verdict = handler
        .handle(
            db.clone(),
            JobPayload::DeployChangeSet {
                task_id: task,
                run_id: run,
            },
        )
        .await;
    assert_eq!(
        verdict,
        hephaestus_engine::HandlerOutcome::Completed,
        "true exits zero: deploy and hooks must pass"
    );
    assert_eq!(
        db.run_scope(run).await.expect("scope").state,
        WorkflowState::Completed
    );

    let finished = db
        .latest_deployment_for_run(org, run)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(finished.status, "succeeded");
    assert!(finished.failure_reason.is_none());

    // Exactly one typed outcome event with verified=true.
    let events: Vec<(bool, String)> = sqlx::query_as(
        "SELECT (payload->'data'->>'verified')::boolean, payload->'data'->>'environment'
         FROM events WHERE aggregate='deployment' AND aggregate_id=$1
           AND payload->>'type'='deployment_outcome'",
    )
    .bind(finished.id)
    .fetch_all(db.pool())
    .await
    .expect("events");
    assert_eq!(events.len(), 1);
    assert!(events[0].0, "verified");
    assert_eq!(events[0].1, "staging");

    // Stale redelivery of the same job is absorbed silently.
    let replay = handler
        .handle(
            db.clone(),
            JobPayload::DeployChangeSet {
                task_id: task,
                run_id: run,
            },
        )
        .await;
    assert_eq!(replay, hephaestus_engine::HandlerOutcome::Completed);
    let again: Vec<(bool,)> = sqlx::query_as(
        "SELECT (payload->'data'->>'verified')::boolean FROM events
         WHERE aggregate='deployment' AND aggregate_id=$1",
    )
    .bind(finished.id)
    .fetch_all(db.pool())
    .await
    .expect("events");
    assert_eq!(again.len(), 1, "no duplicate outcome events on replay");
}

#[tokio::test(flavor = "multi_thread")]
async fn unmatched_target_name_fails_the_run_before_deploying() {
    let _guard = SERIAL.lock().await;
    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);
    seed_to_awaiting_merge(&db, org, task, run, Some("production-eu")).await;

    let settings = Arc::new(staging_settings("true", &["true"]));
    let (verdict, state) =
        merge_and_build(&db, &layout, Arc::clone(&settings), org, task, run).await;
    assert_eq!(
        verdict,
        hephaestus_engine::HandlerOutcome::FailedPermanent,
        "an unresolved target must fail the build stage loudly"
    );
    assert!(
        matches!(state, WorkflowState::Failed),
        "an unresolvable target must fail loudly, not guess"
    );
    assert!(
        db.latest_deployment_for_run(org, run)
            .await
            .expect("read")
            .is_none(),
        "no deployment row may exist for an unresolved name"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_deploy_command_records_evidence_and_fails_run() {
    let _guard = SERIAL.lock().await;
    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);
    seed_to_awaiting_merge(&db, org, task, run, Some("staging")).await;

    let settings = Arc::new(staging_settings("false", &["true"]));
    let (_, state) = merge_and_build(&db, &layout, Arc::clone(&settings), org, task, run).await;
    assert_eq!(state, WorkflowState::Deploying);

    let handler = DeploymentHandler::new(layout.clone(), settings);
    let verdict = handler
        .handle(
            db.clone(),
            JobPayload::DeployChangeSet {
                task_id: task,
                run_id: run,
            },
        )
        .await;
    assert_eq!(
        verdict,
        hephaestus_engine::HandlerOutcome::FailedPermanent,
        "exit 1 is a permanent failure"
    );
    assert_eq!(
        db.run_scope(run).await.expect("scope").state,
        WorkflowState::Failed
    );

    let row = db
        .latest_deployment_for_run(org, run)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.status, "failed");
    let reason = row.failure_reason.expect("failure reason persisted");
    assert!(
        reason.contains("false"),
        "reason names the program: {reason}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_hook_fails_the_run_after_a_clean_deploy() {
    let _guard = SERIAL.lock().await;
    let storage = tempfile::tempdir().expect("storage dir");
    let db = test_db().await;
    let (org, task, run) = seed_task_and_run(&db).await;
    let layout = WorkspaceLayout::new(storage.path());
    make_workspace(storage.path(), run);
    seed_to_awaiting_merge(&db, org, task, run, Some("staging")).await;

    let settings = Arc::new(staging_settings("true", &["true", "false"]));
    let (build_verdict, state) =
        merge_and_build(&db, &layout, Arc::clone(&settings), org, task, run).await;
    eprintln!("build stage verdict: {build_verdict:?}, state: {state:?}");
    assert_eq!(state, WorkflowState::Deploying);

    let handler = DeploymentHandler::new(layout.clone(), settings);
    let verdict = handler
        .handle(
            db.clone(),
            JobPayload::DeployChangeSet {
                task_id: task,
                run_id: run,
            },
        )
        .await;
    assert_eq!(
        verdict,
        hephaestus_engine::HandlerOutcome::FailedPermanent,
        "a failing hook must fail the deployment"
    );
    assert_eq!(
        db.run_scope(run).await.expect("scope").state,
        WorkflowState::Failed
    );

    let row = db
        .latest_deployment_for_run(org, run)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.status, "failed");
    let reason = row.failure_reason.expect("hook failure recorded");
    assert!(reason.contains("false"), "reason names the hook: {reason}");
}
