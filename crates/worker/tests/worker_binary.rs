//! End-to-end test of the `hephaestus-worker` binary.
//!
//! The binary is exercised as a real process against real PostgreSQL,
//! a real local git origin, and a stub OpenAI-compatible endpoint: a
//! task submitted through IntakeService must reach `awaiting_approval`
//! with persisted requirements, an approved-pending plan and an open
//! gate - driven purely by the binary's own registry wiring. A second
//! scenario proves the process refuses to serve model-backed queues
//! without provider configuration instead of retrying into dead-letter.

#![allow(clippy::panic)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use assert_cmd::Command;
use hephaestus_core::domain::{Priority, RiskLevel};
use hephaestus_core::id::{OrganizationId, ProjectId, RepositoryId};
use hephaestus_core::state::WorkflowState;
use hephaestus_db::Db;
use hephaestus_engine::intake::{IntakeRequest, IntakeService};
use hephaestus_repo::git::GitRepo;

/// Same fixture shape the planning pipeline tests pin; documents are
/// stage-valid regardless of which worker serves a stray claim.
const REQUIREMENTS_DOC: &str = r#"{"requirements":[{"category":"functional","kind":"explicit","statement":"Upload succeeds below the configured size limit.","source":"task-description"},{"category":"performance","kind":"assumption","statement":"Production load profile is unknown.","source":"needs load report"}]}"#;

const PLAN_DOC: &str = r#"{"objective":"Fix intermittent 500 on attachment upload.","steps":[{"action":"Add size guard in upload handler src/upload.rs","verification":"unit:upload_size_guard","risks":["behavior change"]},{"action":"Add regression test upload_rejects_oversized","verification":"unit:regression","risks":[]}],"affected_components":["upload"],"affected_symbols":[],"strategy":{"rollback":"revert commit","deployment":null,"verification":["unit"]}}"#;

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

fn db_url() -> String {
    std::env::var("HEPHAESTUS_TEST_DATABASE_URL")
        .unwrap_or_else(|_| panic!("HEPHAESTUS_TEST_DATABASE_URL must be set"))
}

/// Wrap one stage document as the model's final answer under the
/// session protocol ({"final": "<document>"}).
fn final_of(doc: &str) -> String {
    serde_json::json!({ "final": doc }).to_string()
}

fn chat_body(content: &str) -> String {
    serde_json::json!({
        "model": "stub-model",
        "choices": [{
            "message": {"role": "assistant", "content": content}
        }]
    })
    .to_string()
}

/// Find the end of the HTTP header block, if fully buffered.
fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Total request length once Content-Length is known.
fn request_len(buf: &[u8]) -> Option<usize> {
    let head_end = header_end(buf)?;
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let len: usize = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    Some(head_end + len)
}

/// Answer one connection according to its pipeline stage.
///
/// Classification reads the POST body for each stage's stable objective
/// marker, so retries and interleaved runs always receive a valid
/// document for their own stage - no ordering assumptions at all. The
/// FIRST request of a stage gets a governed fs.read tool call instead
/// of a final answer, exercising the audited authorization path end to
/// end through the binary's production AuditLogSink.
fn respond(mut stream: TcpStream, turns: &mut StageTurns) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(total) = request_len(&buf)
                    && buf.len() >= total
                {
                    break;
                }
                if buf.len() > 4 * 1024 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let extraction = text.contains("Extract the engineering requirements");
    let (turn, final_doc) = if extraction {
        turns.extraction += 1;
        (turns.extraction, REQUIREMENTS_DOC)
    } else {
        turns.planning += 1;
        (turns.planning, PLAN_DOC)
    };
    let content = if turn == 1 {
        // Session protocol: a tool-call turn, not the final envelope.
        serde_json::json!({
            "thought": "ground the analysis in the repository",
            "tool": {"name": "fs.read", "args": {"path": "README.md"}}
        })
        .to_string()
    } else {
        final_of(final_doc)
    };
    let body = chat_body(&content);
    let http = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(http.as_bytes());
}

/// Per-stage turn counters for one stub endpoint.
#[derive(Default)]
struct StageTurns {
    /// Requests answered for the extraction stage.
    extraction: u32,
    /// Requests answered for the plan-generation stage.
    planning: u32,
}

/// Bind a stub OpenAI-compatible chat-completions endpoint.
fn spawn_stub() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        let mut turns = StageTurns::default();
        for stream in listener.incoming().flatten() {
            respond(stream, &mut turns);
        }
    });
    port
}

/// Fresh tenant rows pointing at a local git path as remote.
async fn seed_with_local_repo(db: &Db, remote: &str) -> (OrganizationId, ProjectId, RepositoryId) {
    let org = OrganizationId::generate();
    let proj = ProjectId::generate();
    let repo = RepositoryId::generate();
    let slug = format!("bin-{}", org.as_uuid().simple());
    sqlx::query("INSERT INTO organizations (id, name, slug) VALUES ($1,'T',$2)")
        .bind(org.as_uuid())
        .bind(&slug)
        .execute(db.pool())
        .await
        .expect("org");
    sqlx::query("INSERT INTO projects (id, organization_id, name, slug) VALUES ($1,$2,'P',$3)")
        .bind(proj.as_uuid())
        .bind(org.as_uuid())
        .bind(format!("bp-{}", proj.as_uuid().simple()))
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

fn intake_request(org: OrganizationId, proj: ProjectId, repo: RepositoryId) -> IntakeRequest {
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

/// Delete pending backlog jobs whose driving run has been idle for
/// over 90 seconds: leftovers from processes that already exited would
/// otherwise be claimed ahead of this test's jobs.
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

async fn wait_for_state(
    db: &Db,
    run: hephaestus_core::id::WorkflowRunId,
    want: WorkflowState,
) -> bool {
    for _ in 0..600 {
        if let Ok(scope) = db.run_scope(run).await
            && scope.state == want
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_drives_task_from_intake_to_approval_gate() {
    let db = test_db().await;
    let url = db_url();

    // Real local git repository as clone source.
    let origin_dir = tempfile::tempdir().expect("origin dir");
    let git = GitRepo::init(origin_dir.path()).expect("init");
    std::fs::write(
        origin_dir.path().join("README.md"),
        "# Sample\nupload docs\n",
    )
    .expect("seed file");
    git.commit_all("initial", ("tester", "tester@example.invalid"))
        .expect("commit");

    let storage = tempfile::tempdir().expect("storage dir");
    let (org, proj, repo_id) =
        seed_with_local_repo(&db, origin_dir.path().to_str().expect("utf8")).await;

    reap_stale_jobs(&db).await;
    let receipt = IntakeService::new(db.clone())
        .submit(&intake_request(org, proj, repo_id))
        .await
        .expect("intake");

    let port = spawn_stub();
    // CARGO_BIN_EXE_ points at this crate's own binary; a plain
    // process::Child is wanted here so the test controls its lifetime.
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_hephaestus-worker"))
        .env_remove("HEPHAESTUS_CONFIG")
        .env("HEPHAESTUS_DATABASE_URL", &url)
        .env("HEPHAESTUS_STORAGE_ROOT", storage.path())
        .env(
            "HEPHAESTUS_MODEL_BASE_URL",
            format!("http://127.0.0.1:{port}/v1"),
        )
        .env("HEPHAESTUS_MODEL_API_KEY", "integration-test-key")
        .env("HEPHAESTUS_MODEL_NAME", "stub-model")
        .env("HEPHAESTUS_WORKER_QUEUES", "analysis,planning")
        .spawn()
        .expect("spawn worker");

    let reached = wait_for_state(&db, receipt.run_id, WorkflowState::AwaitingApproval).await;
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        reached,
        "binary must drive the run to awaiting_approval; stuck at {:?}",
        db.run_scope(receipt.run_id).await.map(|s| s.state)
    );

    // Requirements persisted with correct epistemics.
    let reqs = db
        .list_requirements(org, receipt.task_id)
        .await
        .expect("reqs");
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].is_gating());
    assert!(!reqs[1].is_gating(), "assumptions must not gate");

    // Plan persisted with ordered steps and planner prompt version.
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

    // Human approval gate open with no decision yet.
    let gate = db
        .get_plan_approval(org, receipt.run_id)
        .await
        .expect("gate row")
        .expect("open gate");
    assert_eq!(gate.gate, "plan");
    assert_eq!(gate.decision, None);

    // Snapshot cloned from the local origin into THIS storage root.
    let snapshot = storage
        .path()
        .join("runs")
        .join(receipt.run_id.to_string())
        .join("repo")
        .join("README.md");
    assert!(snapshot.exists(), "snapshot README must exist");

    // Deterministic analysis/planning events recorded.
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

    // The production audit sink recorded granted tool invocations for
    // governed planner sessions (hash-chained audit log).
    let audited: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_log WHERE actor='agent:planner'
         AND action='tool.invoke' AND target_id='fs.read'
         AND detail->>'decision' = 'granted'",
    )
    .fetch_one(db.pool())
    .await
    .expect("audit count");
    assert!(audited.0 > 0, "planner fs.read decisions must be audited");
}

#[test]
fn refuses_model_backed_queue_without_provider_config() {
    Command::cargo_bin("hephaestus-worker")
        .expect("worker binary built by this crate")
        .env_remove("HEPHAESTUS_CONFIG")
        .env("HEPHAESTUS_DATABASE_URL", "postgres://db.internal:5432/x")
        .env("HEPHAESTUS_WORKER_QUEUES", "analysis,planning")
        .assert()
        .failure()
        .stderr(predicates::str::contains("model.base_url"));
}

/// The two queue vocabularies meet in this crate: configuration
/// validates operator input against WORKER_QUEUE_NAMES, then the
/// binary routes jobs by engine::Queue strings. The lists must agree
/// exactly - config admits every honest queue plus explicitly refuses
/// `deployment`, which the engine still names but nothing may serve.
#[test]
fn config_and_engine_queue_vocabularies_agree() {
    use hephaestus_config::{MODEL_BACKED_QUEUES, WORKER_QUEUE_NAMES};

    let engine_queues: Vec<&str> = hephaestus_engine::jobs::Queue::iter_all()
        .map(|q| q.as_str())
        .collect();
    let mut configurable: Vec<&str> = WORKER_QUEUE_NAMES.to_vec();
    configurable.push("deployment");
    configurable.sort_unstable();
    let mut engine_sorted = engine_queues.clone();
    engine_sorted.sort_unstable();
    assert_eq!(configurable, engine_sorted);

    // Every model-backed name must itself be configurable.
    for q in MODEL_BACKED_QUEUES {
        assert!(WORKER_QUEUE_NAMES.contains(&q));
    }
}
