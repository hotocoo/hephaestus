//! Delivery: the merge gate and the deterministic build stage
//! (ADR-008).
//!
//! Review parks finished work at `awaiting_merge`; merging itself is
//! genuinely external - humans or CI accept the change set on the
//! platform where the code lives. This module records that decision
//! on the run's open `merge` approval gate and chains the one thing
//! Hephaestus can do deterministically afterwards: build the change
//! set and either complete the run (no deployment demanded) or fail
//! loudly (a deployment was demanded but no deployment executor
//! exists).
//!
//! Nothing here simulates success. A plan that names a deployment
//! strategy fails the run at this stage until real target
//! configuration lands with the deployment phase; pretending the
//! deployment happened would violate the repository's no-simulation
//! contract. Likewise there is no automatic merge anywhere: the state
//! machine only leaves `awaiting_merge` through an explicitly
//! recorded external decision.

use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use hephaestus_agent::role::{AgentRole, RoleManifest};
use hephaestus_core::domain::StrategyNotes;
use hephaestus_core::id::{BuildId, OrganizationId, TaskId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_core::{Error, Result};
use hephaestus_db::Db;
use hephaestus_tools::SandboxedShell;

use crate::analysis::{StageError, WorkspaceLayout, chain_job};
use crate::deployment::DeploymentSettings;
use crate::jobs::{JobPayload, Queue};
use crate::worker::{HandlerOutcome, JobHandler};

/// The pinned build command. Like verification layers (ADR-007),
/// building evolves only through reviewed changes to this mapping,
/// keeping the delivery spec auditable.
pub const BUILD_COMMAND: (&str, &[&str]) = ("cargo", &["build", "--workspace"]);

/// Where deterministic artifacts are collected, relative to the run
/// workspace. Paths stored in evidence are always workspace-relative,
/// never absolute.
pub const ARTIFACT_DIR: &str = "target/debug";

/// Wall-clock cap for one build, matching the verification layers.
pub const BUILD_TIMEOUT_SECS: u64 = 900;

/// An externally made merge decision, recorded verbatim.
#[derive(Debug, Clone)]
pub struct MergeDecisionInput {
    /// Who merged (authenticated principal or CI identity). Recorded
    /// in the event log; never inferred.
    pub merged_by: String,
    /// Free-form rationale, persisted with the decision.
    pub reason: Option<String>,
    /// External reference for the merge (commit sha, PR number, ...).
    /// Evidence only; Hephaestus never fetches it.
    pub external_ref: Option<String>,
}

/// What happened after a merge decision was durably applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The decision was recorded now; the run advanced to `building`
    /// and the build job was chained.
    Merged,
    /// The effects of an earlier identical call were already visible;
    /// nothing was double-spent and the chain was left consistent.
    AlreadyMerged,
}

/// Records external merge decisions and bootstraps the build stage.
///
/// Recovery contract (mirrors ADR-007): recording the gate decision,
/// creating the build row, advancing the state machine and chaining
/// the build job are separate durable steps, each individually
/// idempotent. Re-issuing the same decision after a crash between any
/// two of them resumes the chain instead of double-spending it.
#[derive(Debug, Clone, Copy, Default)]
pub struct MergeService;

impl MergeService {
    /// Apply one merge decision to the run's open merge gate.
    pub async fn decide(
        &self,
        db: &Db,
        org: OrganizationId,
        run: WorkflowRunId,
        input: &MergeDecisionInput,
    ) -> Result<MergeOutcome> {
        validate_decision(input)?;

        let scope = db.run_scope(run).await?;
        let mut fresh_decision = false;

        match scope.state {
            // Terminal: everything an earlier call did stays visible;
            // re-driving anything would only churn durable history.
            WorkflowState::Completed | WorkflowState::Failed | WorkflowState::Cancelled => {
                return Ok(MergeOutcome::AlreadyMerged);
            }
            // The transition happened but the job chain may have been
            // interrupted; fall through and re-drive idempotently.
            WorkflowState::Building => {
                db.active_build_for_run(org, run)
                    .await?
                    .ok_or(Error::Conflict {
                        message: "run is building without an active build row".into(),
                    })?;
                chain_build_job(db, scope.task_id, run).await?;
                return Ok(MergeOutcome::AlreadyMerged);
            }
            WorkflowState::AwaitingMerge => {}
            other => {
                return Err(Error::Conflict {
                    message: format!(
                        "run is {}, not awaiting_merge; no merge can be recorded",
                        other.name()
                    ),
                });
            }
        }

        let gate = db.get_merge_gate(org, run).await?.ok_or(Error::Conflict {
            message: "no merge gate exists for this run".into(),
        })?;
        if gate.decision.is_none() {
            db.decide_merge_gate(
                org,
                run,
                &input.merged_by,
                composed_reason(input).as_deref(),
            )
            .await?;
            fresh_decision = true;
        }
        // A decided-but-unadvanced gate means a previous call crashed
        // after step one: resume below instead of refusing.

        let exec = db
            .passed_execution_for_run(org, run)
            .await?
            .ok_or(Error::Validation {
                field: "execution".into(),
                message: "run awaits merge but has no passed execution".into(),
            })?;
        // Bootstrap BEFORE the transition: creation is idempotent per
        // run, so a crash here replays cleanly through this function.
        db.create_build_for_run(
            org,
            scope.task_id,
            run,
            hephaestus_core::id::ExecutionId::from_uuid(exec.id),
        )
        .await?;
        if scope.state == WorkflowState::AwaitingMerge {
            db.transition_run(
                org,
                run,
                WorkflowState::AwaitingMerge,
                TransitionEvent::Merged,
                "merge-service",
            )
            .await?;
        }
        chain_build_job(db, scope.task_id, run).await?;

        Ok(if fresh_decision {
            MergeOutcome::Merged
        } else {
            MergeOutcome::AlreadyMerged
        })
    }
}

/// Compose the persisted rationale from reason and external ref.
fn composed_reason(input: &MergeDecisionInput) -> Option<String> {
    match (
        input
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        input
            .external_ref
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (Some(reason), Some(reference)) => Some(format!("{reason}; ref={reference}")),
        (Some(reason), None) => Some(reason.to_string()),
        (None, Some(reference)) => Some(format!("ref={reference}")),
        (None, None) => None,
    }
}

/// Reject decisions with no attributable actor before any durable
/// effect: unattributed merges would poison the audit trail.
fn validate_decision(input: &MergeDecisionInput) -> Result<()> {
    if input.merged_by.trim().is_empty() {
        return Err(Error::Validation {
            field: "merged_by".into(),
            message: "merge decision requires a principal".into(),
        });
    }
    Ok(())
}

/// Whether the plan demands a deployment after the build.
///
/// Whitespace-only notes count as absent: a planner leaving the field
/// effectively empty must not block completion.
pub fn wants_deployment(strategy: &StrategyNotes) -> bool {
    strategy
        .deployment
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
}

/// SHA-256 over the executable files under `dir`, ordered by path.
///
/// Each file contributes its workspace-relative path, a NUL separator,
/// its content, and another NUL, so neither concatenation nor reorder
/// ambiguities can forge a collision. Returns the hex digest and the
/// file count; an empty directory hashes the empty input (still a
/// stable, honest fingerprint of "nothing executable was produced").
pub fn hash_artifacts(dir: &Path) -> Result<(String, usize)> {
    let (digest, files) = collect_artifacts(dir)?;
    Ok((digest, files.len()))
}

/// One produced artifact file, hashed individually (ADR-015).
#[derive(Debug, Clone)]
pub struct ArtifactFile {
    /// Path relative to the artifact directory.
    pub relative_path: String,
    /// SHA-256 over the file content (hex).
    pub sha256: String,
    /// File size in bytes.
    pub size_bytes: u64,
}

/// Single pass over the produced executables yielding both the
/// aggregate directory fingerprint (exactly the ADR-008 algorithm) and
/// the per-file registry records, so one read per file serves both.
pub fn collect_artifacts(dir: &Path) -> Result<(String, Vec<ArtifactFile>)> {
    let mut executables: Vec<std::path::PathBuf> = WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| is_executable(p))
        .collect();
    executables.sort();
    executables.dedup();

    let mut hasher = Sha256::new();
    let mut files = Vec::with_capacity(executables.len());
    for path in &executables {
        let relative = path
            .strip_prefix(dir)
            .map_err(|e| Error::Storage(Box::new(e)))?;
        let bytes = std::fs::read(path).map_err(|e| Error::Storage(Box::new(e)))?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        hasher.update([0u8]);
        let mut content = Sha256::new();
        content.update(&bytes);
        files.push(ArtifactFile {
            relative_path: relative.to_string_lossy().into_owned(),
            sha256: hex::encode(content.finalize()),
            size_bytes: bytes.len() as u64,
        });
    }
    let digest = hasher.finalize();
    Ok((hex::encode(digest), files))
}

/// Executable-bit probe (unix permission model).
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| m.mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Enqueue the build job; the key makes redelivery a no-op.
async fn chain_build_job(db: &Db, task: TaskId, run: WorkflowRunId) -> Result<()> {
    chain_job(
        db,
        Queue::Build.as_str(),
        &format!("build-artifact:{run}"),
        run,
        JobPayload::BuildArtifact {
            task_id: task,
            run_id: run,
        },
    )
    .await
}

/// Advance Building -> Failed after a terminal build-stage problem,
/// absorbing the lost race a concurrent redelivery may have won.
async fn fail_run(
    db: &Db,
    org: OrganizationId,
    run: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    if let Err(e) = db
        .transition_run(
            org,
            run,
            WorkflowState::Building,
            TransitionEvent::Fail,
            "build-handler",
        )
        .await
        && !matches!(e, Error::Conflict { .. })
    {
        return Err(StageError::Retryable(e));
    }
    Ok(())
}

/// Route a successful build to its next legal hop: complete via
/// skip-deployment when the plan leaves deployment unset; otherwise
/// bootstrap the deployment row and chain the deploy job (ADR-013).
///
/// Every step re-derives its necessity from visible state, so a crash
/// anywhere in here replays cleanly through a redelivered build job.
async fn route_built_change(
    db: &Db,
    org: OrganizationId,
    task: TaskId,
    run: WorkflowRunId,
    build: hephaestus_db::delivery::BuildRow,
    deployments: Option<&DeploymentSettings>,
) -> std::result::Result<(), StageError> {
    let plan = db
        .get_current_plan(org, task)
        .await
        .map_err(StageError::Permanent)?
        .ok_or_else(|| StageError::Permanent(Error::NotFound { entity: "plan" }))?;
    let execution = db
        .get_execution(
            org,
            hephaestus_core::id::ExecutionId::from_uuid(build.execution_id),
        )
        .await
        .map_err(StageError::Permanent)?;
    if plan.id.as_uuid() != execution.plan_id {
        return Err(StageError::Permanent(Error::Validation {
            field: "plan".into(),
            message: "current plan does not match the executed plan".into(),
        }));
    }

    if !wants_deployment(&plan.strategy) {
        db.transition_run(
            org,
            run,
            WorkflowState::Building,
            TransitionEvent::SkipDeployment,
            "build-handler",
        )
        .await
        .map_err(StageError::Retryable)?;
        return Ok(());
    }

    // The plan demands a deployment. Without configured targets that is
    // still the loud ADR-008 refusal - claiming completion would be
    // simulation, and so would inventing a target.
    let Some(settings) = deployments else {
        fail_run(db, org, run).await?;
        return Err(StageError::Permanent(Error::Validation {
            field: "strategy.deployment".into(),
            message: "plan demands a deployment but no deployment executor exists;                  the built change set is preserved as evidence"
                .into(),
        }));
    };
    let note = plan.strategy.deployment.as_deref().unwrap_or("").trim();
    let Some(target) = settings.resolve(note) else {
        fail_run(db, org, run).await?;
        return Err(StageError::Permanent(Error::Validation {
            field: "strategy.deployment".into(),
            message: format!(
                "plan names deployment target {note:?} but configuration has: {}",
                settings.names()
            ),
        }));
    };

    // Bootstrap BEFORE the transition (mirroring builds and
    // executions): creation is idempotent per run, so a crash here
    // replays cleanly through this function.
    db.create_deployment_for_build(org, task, run, BuildId::from_uuid(build.id), &target.name)
        .await
        .map_err(StageError::Permanent)?;
    // Building -> Deploying. A Conflict means another redelivery won
    // the race and advanced already; its effects are exactly ours.
    if let Err(e) = db
        .transition_run(
            org,
            run,
            WorkflowState::Building,
            TransitionEvent::BuildSucceeded,
            "build-handler",
        )
        .await
        && !matches!(e, Error::Conflict { .. })
    {
        return Err(StageError::Retryable(e));
    }
    chain_deploy_job(db, task, run)
        .await
        .map_err(StageError::Retryable)?;
    Ok(())
}

/// Enqueue the deployment job; the key makes redelivery a no-op.
/// `chain_job` appends the run id, so the durable key reads
/// `deploy:<run>`.
async fn chain_deploy_job(db: &Db, task: TaskId, run: WorkflowRunId) -> Result<()> {
    chain_job(
        db,
        Queue::Deployment.as_str(),
        "deploy",
        run,
        JobPayload::DeployChangeSet {
            task_id: task,
            run_id: run,
        },
    )
    .await
}

async fn build(
    db: &Db,
    layout: &WorkspaceLayout,
    deployments: Option<&DeploymentSettings>,
    task_id: TaskId,
    run_id: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    let scope = db.run_scope(run_id).await.map_err(StageError::Permanent)?;
    let org = scope.organization_id;
    // Stale redelivery after any terminal hop is absorbed silently.
    if matches!(
        scope.state,
        WorkflowState::Completed | WorkflowState::Failed | WorkflowState::Cancelled
    ) {
        return Ok(());
    }
    // Crash after BuildSucceeded: the build is durable, so re-drive
    // only the interrupted routing instead of rebuilding or dying.
    if matches!(
        scope.state,
        WorkflowState::Deploying | WorkflowState::VerifyingDeployment
    ) {
        let latest = db
            .latest_build_for_run(org, run_id)
            .await
            .map_err(StageError::Permanent)?
            .ok_or_else(|| {
                StageError::Permanent(Error::Conflict {
                    message: "post-build run without any build row".into(),
                })
            })?;
        return route_built_change(db, org, task_id, run_id, latest, deployments).await;
    }
    match scope.state {
        WorkflowState::Building => {}
        other => {
            return Err(StageError::Permanent(Error::Conflict {
                message: format!("run is {}, not building", other.name()),
            }));
        }
    }
    let row = match db.active_build_for_run(org, run_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            // A crash may have landed between finishing the build and
            // routing it; a succeeded row means only routing remains.
            let latest = db
                .latest_build_for_run(org, run_id)
                .await
                .map_err(StageError::Permanent)?;
            match latest {
                Some(latest) if latest.status == "succeeded" => {
                    return route_built_change(db, org, task_id, run_id, latest, deployments).await;
                }
                _ => {
                    return Err(StageError::Permanent(Error::Conflict {
                        message: "run is building without an active build row".into(),
                    }));
                }
            }
        }
        Err(e) => return Err(StageError::Permanent(e)),
    };
    let build_id = BuildId::from_uuid(row.id);

    // Verifier capabilities: read + allowlisted cargo. The change set
    // is frozen post-merge; the build writes only under target/.
    let workspace = layout.run_repo_dir(run_id);
    let caps = RoleManifest::built_in(AgentRole::Verifier).capability_set(workspace.clone());
    let shell =
        SandboxedShell::new(&caps).with_timeout(std::time::Duration::from_secs(BUILD_TIMEOUT_SECS));
    let outcome = shell
        .execute(BUILD_COMMAND.0, BUILD_COMMAND.1)
        .map_err(StageError::Retryable)?;

    if outcome.timed_out || outcome.exit_code != Some(0) {
        db.finish_build(org, run_id, build_id, false, None, None)
            .await
            .map_err(StageError::Permanent)?;
        fail_run(db, org, run_id).await?;
        return Err(StageError::Permanent(Error::Validation {
            field: "build".into(),
            message: format!(
                "{} {} failed (exit_code={:?}, timed_out={})",
                BUILD_COMMAND.0,
                BUILD_COMMAND.1.join(" "),
                outcome.exit_code,
                outcome.timed_out
            ),
        }));
    }

    let artifact_dir = workspace.join(ARTIFACT_DIR);
    let (sha, files) = collect_artifacts(&artifact_dir).map_err(StageError::Retryable)?;
    let artifact_path = if files.is_empty() {
        None
    } else {
        Some(ARTIFACT_DIR)
    };
    // Registry rows (workspace-relative: target/debug/<file>) land in
    // the same transaction as the build row's terminal outcome, so
    // state and per-file evidence never diverge (ADR-015).
    let records: Vec<hephaestus_db::delivery::ArtifactRecord> = files
        .iter()
        .map(|file| hephaestus_db::delivery::ArtifactRecord {
            task_id,
            path: format!("{ARTIFACT_DIR}/{}", file.relative_path),
            sha256: file.sha256.clone(),
            size_bytes: file.size_bytes,
        })
        .collect();
    db.finish_build_with_artifacts(
        org,
        run_id,
        build_id,
        true,
        artifact_path,
        Some(&sha),
        &records,
    )
    .await
    .map_err(StageError::Permanent)?;

    // Re-read the row so routing sees its terminal, evidence-grade form.
    let finished = db
        .latest_build_for_run(org, run_id)
        .await
        .map_err(StageError::Permanent)?
        .ok_or_else(|| {
            StageError::Permanent(Error::Conflict {
                message: "build row vanished after finishing".into(),
            })
        })?;
    route_built_change(db, org, task_id, run_id, finished, deployments).await
}

/// Handler for the build stage of the delivery pipeline.
pub struct BuildHandler {
    layout: WorkspaceLayout,
    deployments: Option<Arc<DeploymentSettings>>,
}

impl BuildHandler {
    /// Bind the workspace layout. Without deployment settings a plan
    /// demanding deployment fails loudly (ADR-008 semantics).
    pub fn new(layout: WorkspaceLayout) -> Self {
        Self {
            layout,
            deployments: None,
        }
    }

    /// Configure deployment targets (ADR-013). Absent settings keep
    /// the loud no-executor refusal for runs that demand deployment.
    pub fn with_deployments(mut self, settings: Arc<DeploymentSettings>) -> Self {
        self.deployments = Some(settings);
        self
    }
}

impl JobHandler for BuildHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::BuildArtifact { task_id, run_id } = payload else {
                tracing::error!(?payload, "build handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match build(
                &db,
                &self.layout,
                self.deployments.as_deref(),
                task_id,
                run_id,
            )
            .await
            {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(run = %run_id, error = %e, "build retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(run = %run_id, error = %e, "build permanent failure");
                    HandlerOutcome::FailedPermanent
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn build_command_is_pinned_to_cargo_workspace() {
        assert_eq!(BUILD_COMMAND.0, "cargo");
        assert_eq!(BUILD_COMMAND.1, &["build", "--workspace"]);
    }

    #[test]
    fn decisions_require_a_principal() {
        let ok = validate_decision(&MergeDecisionInput {
            merged_by: "ci-bot".into(),
            reason: None,
            external_ref: Some("abc123".into()),
        });
        assert!(ok.is_ok());

        let blank = validate_decision(&MergeDecisionInput {
            merged_by: "   ".into(),
            reason: None,
            external_ref: None,
        });
        assert!(matches!(blank, Err(Error::Validation { field, .. }) if field == "merged_by"));
    }

    #[test]
    fn reason_composition_keeps_both_facts() {
        let both = composed_reason(&MergeDecisionInput {
            merged_by: "p".into(),
            reason: Some("  looked good ".into()),
            external_ref: Some(" PR #12 ".into()),
        });
        assert_eq!(both.as_deref(), Some("looked good; ref=PR #12"));

        let only_ref = composed_reason(&MergeDecisionInput {
            merged_by: "p".into(),
            reason: None,
            external_ref: Some("deadbeef".into()),
        });
        assert_eq!(only_ref.as_deref(), Some("ref=deadbeef"));

        let none = composed_reason(&MergeDecisionInput {
            merged_by: "p".into(),
            reason: None,
            external_ref: None,
        });
        assert_eq!(none, None);
    }

    #[test]
    fn deployment_demand_reads_the_strategy_notes() {
        assert!(!wants_deployment(&StrategyNotes::default()));
        assert!(!wants_deployment(&StrategyNotes {
            rollback: None,
            deployment: Some("   ".into()),
            verification: vec![],
        }));
        assert!(wants_deployment(&StrategyNotes {
            rollback: None,
            deployment: Some("ship via internal CD".into()),
            verification: vec![],
        }));
    }

    #[test]
    fn artifact_hash_is_order_independent_and_content_sensitive() {
        let dir = tempfile::tempdir().expect("tmp");
        let b = dir.path().join("b.bin");
        let a = dir.path().join("a.bin");
        std::fs::write(&b, b"two").expect("write");
        std::fs::write(&a, b"one").expect("write");
        make_executable(&b);
        make_executable(&a);

        let first = hash_artifacts(dir.path()).expect("hash");
        assert_eq!(first.1, 2);
        // Same tree hashed again must agree byte-for-byte.
        let second = hash_artifacts(dir.path()).expect("hash");
        assert_eq!(first.0, second.0);

        // Touching content changes the digest.
        std::fs::write(&a, b"ONE").expect("rewrite");
        let third = hash_artifacts(dir.path()).expect("hash");
        assert_ne!(first.0, third.0);
    }

    /// Mark a fixture executable so it qualifies for hashing.
    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[cfg(not(unix))]
    fn make_executable(path: &Path) {
        let _ = path;
    }

    #[test]
    fn non_executable_files_never_enter_the_digest() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("notes.txt"), b"ignore me").expect("write");
        let empty = hash_artifacts(dir.path()).expect("hash");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bin = dir.path().join("app");
            std::fs::write(
                &bin,
                b"#!/bin/sh
",
            )
            .expect("write");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            let with_bin = hash_artifacts(dir.path()).expect("hash");
            assert_eq!(with_bin.1, 1);
            assert_ne!(empty.0, with_bin.0);
        }
        assert_eq!(empty.1, 0);
    }

    #[test]
    fn empty_artifact_dir_still_hashes_stably() {
        let dir = tempfile::tempdir().expect("tmp");
        let a = hash_artifacts(dir.path()).expect("hash");
        let b = hash_artifacts(dir.path()).expect("hash");
        assert_eq!(a, b);
        assert_eq!(a.1, 0);
    }
}
