//! Repository analysis handler.
//!
//! The first stage of every run: obtain a local snapshot of the target
//! repository and derive deterministic facts about it. Snapshotting
//! goes through hephaestus-repo's hardened git integration (argument
//! vectors, disabled hooks/prompting); inventory comes from the same
//! crate's tracked-file scanner. No model involvement: everything here
//! is deterministic and its provenance is `computed`.
//!
//! On success the handler chains the requirement-extraction job so the
//! workflow continues without any central orchestrator process.

use std::path::{Path, PathBuf};

use serde_json::json;

use hephaestus_core::id::{TaskId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_core::{Error, Result};
use hephaestus_db::Db;
use hephaestus_repo::git::GitRepo;
use hephaestus_repo::inventory;

use crate::jobs::{JobPayload, Queue};
use crate::worker::{HandlerOutcome, JobHandler};

/// Failure classification for pipeline stages: transient problems get
/// queue retries with backoff; configuration/data problems never will.
#[derive(Debug)]
pub enum StageError {
    /// Retry later (network, provider hiccups, protocol noise).
    Retryable(Error),
    /// Never retry (missing data, illegal state, bad configuration).
    Permanent(Error),
}

/// Where run workspaces live.
///
/// Layout: `<root>/runs/<run_id>/repo`. Operators point this at the
/// configured storage root; tests point it at a tempdir.
#[derive(Debug, Clone)]
pub struct WorkspaceLayout {
    /// Storage root directory.
    pub root: PathBuf,
}

impl WorkspaceLayout {
    /// Layout under one storage root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The repository snapshot directory for one run.
    pub fn run_repo_dir(&self, run: WorkflowRunId) -> PathBuf {
        self.root.join("runs").join(run.to_string()).join("repo")
    }
}

/// Handler for [`JobPayload::AnalyzeRepository`].
///
/// Stateless besides its layout; all coordination happens through the
/// durable queue and database, so redelivery is safe.
pub struct AnalysisHandler {
    layout: WorkspaceLayout,
}

impl AnalysisHandler {
    /// Bind the handler to a workspace layout.
    pub fn new(layout: WorkspaceLayout) -> Self {
        Self { layout }
    }

    /// Snapshot directory for one run (where read-only agent sessions
    /// may inspect sources).
    pub fn repo_dir(&self, run: WorkflowRunId) -> PathBuf {
        self.layout.run_repo_dir(run)
    }

    /// Reuse an existing intact snapshot or clone fresh. Redelivery
    /// after a crash between clone and complete must not re-clone over
    /// a valid checkout (git refuses non-empty destinations anyway).
    fn open_or_clone(&self, remote: &str, dest: &Path) -> Result<GitRepo> {
        if dest.join(".git").exists() {
            return GitRepo::open(dest);
        }
        GitRepo::clone_into(remote, dest)
    }
}

impl JobHandler for AnalysisHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::AnalyzeRepository { task_id, run_id } = payload else {
                tracing::error!(?payload, "analysis handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match analyze(&db, self, task_id, run_id).await {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(task = %task_id, run = %run_id, error = %e, "analysis retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(task = %task_id, run = %run_id, error = %e, "analysis permanent failure");
                    HandlerOutcome::FailedPermanent
                }
            }
        })
    }
}

async fn analyze(
    db: &Db,
    handler: &AnalysisHandler,
    task_id: TaskId,
    run_id: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    let scope = db.run_scope(run_id).await.map_err(StageError::Permanent)?;

    // Advance created -> analyzing. On redelivery the CAS conflict is
    // the expected, benign outcome; anything else propagates.
    if scope.state == WorkflowState::Created
        && let Err(e) = db
            .transition_run(
                scope.organization_id,
                run_id,
                WorkflowState::Created,
                TransitionEvent::StartAnalysis,
                "analysis-handler",
            )
            .await
        && !matches!(e, Error::Conflict { .. })
    {
        return Err(StageError::Retryable(e));
    }

    let task = db
        .get_task(scope.organization_id, task_id)
        .await
        .map_err(StageError::Permanent)?;
    let remote = db
        .repository_remote(
            scope.organization_id,
            hephaestus_core::id::RepositoryId::from_uuid(task.repository_id),
        )
        .await
        .map_err(StageError::Permanent)?;

    // Untrusted repository content: cloned through hardened git only.
    let dest = handler.repo_dir(run_id);
    let repo = handler
        .open_or_clone(&remote, &dest)
        .map_err(StageError::Retryable)?;

    let head = repo.head_commit().map_err(StageError::Retryable)?;
    let entries = inventory::scan_tracked(&repo).map_err(StageError::Retryable)?;
    let languages = inventory::summarize(&entries)
        .into_iter()
        .map(|(lang, files)| json!({ "language": lang, "files": files }))
        .collect::<Vec<_>>();

    let summary = json!({
        "type": "repository_analyzed",
        "data": {
            "run_id": run_id.to_string(),
            "head": head,
            "tracked_files": entries.len(),
            "languages": languages,
        }
    });
    db.append_task_event_for_run(scope.organization_id, task_id, run_id, "computed", summary)
        .await
        .map_err(StageError::Retryable)?;

    chain_job(
        db,
        Queue::Analysis.as_str(),
        "extract-requirements",
        run_id,
        JobPayload::ExtractRequirements { task_id, run_id },
    )
    .await
    .map_err(StageError::Retryable)?;
    Ok(())
}

/// Enqueue the next stage's job. The idempotency key makes redelivery
/// after a crash between enqueue and job-complete a no-op.
pub(crate) async fn chain_job(
    db: &Db,
    queue: &str,
    key_prefix: &str,
    run_id: WorkflowRunId,
    payload: JobPayload,
) -> Result<()> {
    let envelope = payload
        .to_envelope()
        .map_err(|e| Error::Storage(Box::new(e)))?;
    let key = format!("{key_prefix}:{run_id}");
    db.enqueue(queue, &envelope, 0, Some(&key)).await?;
    Ok(())
}
