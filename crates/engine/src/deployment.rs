//! Deployment: configured targets, operator role, post-deploy
//! verification (ADR-013).
//!
//! When a build succeeds and the plan demands deployment, the build
//! stage bootstraps the running `deployments` row, advances
//! `building -> deploying` and chains one job on the `deployment`
//! queue. This module owns that queue's handler: it executes the
//! configured deploy command under an operator manifest derived from
//! the target's own commands, then runs the mandatory verification
//! hooks - each phase a separate idempotent step that re-derives its
//! necessity from visible durable state.
//!
//! Nothing here simulates success. A strategy note naming no
//! configured target fails the run before any command runs; a failed
//! deploy or a failing hook finishes the row as failed, persists the
//! reason, appends the typed event and fails the run terminally.

use std::sync::Arc;
use std::time::Duration;

use hephaestus_agent::role::RoleManifest;
use hephaestus_core::id::{DeploymentId, OrganizationId, TaskId, WorkflowRunId};
use hephaestus_core::state::{TransitionEvent, WorkflowState};
use hephaestus_core::{Error, Result};
use hephaestus_db::Db;
use hephaestus_tools::SandboxedShell;
use hephaestus_tools::registry::ToolRegistry;

use crate::analysis::{StageError, WorkspaceLayout};
use crate::jobs::JobPayload;
use crate::worker::{HandlerOutcome, JobHandler};

/// One allowlisted argv vector: bare program name first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommand {
    /// Bare executable name resolved on PATH inside the workspace.
    pub program: String,
    /// Arguments passed through verbatim.
    pub args: Vec<String>,
}

impl DeployCommand {
    /// Split an argv vector into a command. The first element is the
    /// program; configuration validation rejects empty vectors before
    /// this ever runs, so a failure here is defensive, not expected.
    pub fn from_argv(argv: &[String]) -> Result<Self> {
        let (program, args) = argv.split_first().ok_or(Error::Validation {
            field: "deployment.command".into(),
            message: "command must hold at least a program name".into(),
        })?;
        Ok(Self {
            program: program.clone(),
            args: args.to_vec(),
        })
    }
}

/// One configured deployment target as the engine consumes it.
/// Converted from `hephaestus_config::DeploymentTarget` by callers;
/// validation already ran at config load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentTargetSpec {
    /// Name plans cite in `strategy.deployment`.
    pub name: String,
    /// The deploy command.
    pub command: DeployCommand,
    /// Mandatory post-deployment verification hooks, in order.
    pub verify: Vec<DeployCommand>,
    /// Wall-clock cap for the deploy command and for each hook.
    pub timeout_secs: u64,
}

impl DeploymentTargetSpec {
    /// Every program this target may execute - exactly the surface of
    /// the operator manifest this stage derives and validates.
    fn command_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.command.program.as_str())
            .chain(self.verify.iter().map(|h| h.program.as_str()))
    }
}

/// Configured targets a worker can deploy to. Empty means this
/// process never deploys: build-stage routing falls back to ADR-008's
/// loud refusal, exactly as before executors existed.
#[derive(Debug, Clone, Default)]
pub struct DeploymentSettings {
    /// Configured targets, in configuration order.
    pub targets: Vec<DeploymentTargetSpec>,
}

impl DeploymentSettings {
    /// Settings over one or more targets.
    pub fn new(targets: Vec<DeploymentTargetSpec>) -> Self {
        Self { targets }
    }

    /// Resolve a plan's strategy note to a configured target by exact
    /// name. No prose parsing, no default target: an unmatched note is
    /// a loud validation failure listing what IS configured.
    pub fn resolve(&self, strategy_note: &str) -> Option<&DeploymentTargetSpec> {
        let name = strategy_note.trim();
        self.targets.iter().find(|t| t.name == name)
    }

    /// Comma-joined names for failure messages.
    pub fn names(&self) -> String {
        if self.targets.is_empty() {
            "none configured".to_string()
        } else {
            self.targets
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

/// Handler for the `deployment` queue (ADR-013).
pub struct DeploymentHandler {
    layout: WorkspaceLayout,
    settings: Arc<DeploymentSettings>,
    registry: ToolRegistry,
}

impl DeploymentHandler {
    /// Bind the workspace layout and configured targets.
    pub fn new(layout: WorkspaceLayout, settings: Arc<DeploymentSettings>) -> Self {
        Self {
            layout,
            settings,
            registry: ToolRegistry::with_builtins(),
        }
    }
}

impl JobHandler for DeploymentHandler {
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>> {
        Box::pin(async move {
            let JobPayload::DeployChangeSet { task_id, run_id } = payload else {
                tracing::error!(?payload, "deployment handler received wrong payload kind");
                return HandlerOutcome::FailedPermanent;
            };
            match deploy(&db, self, task_id, run_id).await {
                Ok(()) => HandlerOutcome::Completed,
                Err(StageError::Retryable(e)) => {
                    tracing::warn!(run = %run_id, error = %e, "deployment retryable failure");
                    HandlerOutcome::Retryable
                }
                Err(StageError::Permanent(e)) => {
                    tracing::error!(run = %run_id, error = %e, "deployment permanent failure");
                    HandlerOutcome::FailedPermanent
                }
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Run the deploy command, then advance to verification.
    Deploy,
    /// Deploy already finished durably; run the hooks and complete.
    Verify,
    /// Hooks already passed durably; only the completion transition
    /// remains (a crash landed between finish and transition).
    CompleteTransition,
}

async fn deploy(
    db: &Db,
    handler: &DeploymentHandler,
    _task_id: TaskId,
    run_id: WorkflowRunId,
) -> std::result::Result<(), StageError> {
    let scope = db.run_scope(run_id).await.map_err(StageError::Permanent)?;
    let org = scope.organization_id;

    // Stale redelivery after any terminal hop is absorbed silently:
    // every effect it could double-spend is already visible.
    if matches!(
        scope.state,
        WorkflowState::Completed | WorkflowState::Failed | WorkflowState::Cancelled
    ) {
        return Ok(());
    }

    let active = db
        .active_deployment_for_run(org, run_id)
        .await
        .map_err(StageError::Permanent)?;

    let (target_name, dep, phase) = match (scope.state, active) {
        (WorkflowState::Deploying, Some(row)) => (row.target, row.id, Phase::Deploy),
        (WorkflowState::VerifyingDeployment, Some(row)) => (row.target, row.id, Phase::Verify),
        // No active row under a live deployment phase means a crash
        // landed between finishing the row and its transition. Re-drive
        // just the hop when the row succeeded; complete a recorded
        // failure loudly; refuse shapes that should not exist.
        (state @ (WorkflowState::Deploying | WorkflowState::VerifyingDeployment), None) => {
            let latest = db
                .latest_deployment_for_run(org, run_id)
                .await
                .map_err(StageError::Permanent)?;
            match latest {
                Some(row)
                    if row.status == "succeeded" && state == WorkflowState::VerifyingDeployment =>
                {
                    (row.target, row.id, Phase::CompleteTransition)
                }
                Some(row) => {
                    fail_run(db, org, run_id, state).await?;
                    return Err(StageError::Permanent(Error::Validation {
                        field: "deployment".into(),
                        message: format!("deployment {} was recorded earlier", row.status),
                    }));
                }
                None => {
                    fail_run(db, org, run_id, state).await?;
                    return Err(StageError::Permanent(Error::Conflict {
                        message: format!("run is {} without any deployment row", state.name()),
                    }));
                }
            }
        }
        other => {
            return Err(StageError::Permanent(Error::Conflict {
                message: format!("run is {}, not deploying", other.0.name()),
            }));
        }
    };

    let dep_id = DeploymentId::from_uuid(dep);
    let ctx = CmdRun {
        db,
        handler,
        org,
        run_id,
        dep: dep_id,
    };
    let target = resolve_target(handler, db, org, run_id, scope.state, &target_name).await?;

    // One job drives the whole deployment: deploy, then verify. Each
    // step is individually durable and re-derivable, so a crash anywhere
    // in here resumes from visible state on redelivery instead of
    // re-running finished phases.
    if phase == Phase::Deploy {
        run_command(&ctx, &target, scope.state, &target.command).await?;
        db.transition_run(
            org,
            run_id,
            WorkflowState::Deploying,
            TransitionEvent::DeploymentFinished,
            "deployment-handler",
        )
        .await
        .map_err(StageError::Retryable)?;
    }
    if phase != Phase::CompleteTransition {
        for hook in &target.verify {
            // Hooks run in order; the first failure ends the deployment
            // and the run. Redelivery can re-execute hooks -
            // at-least-once semantics operators writing hooks sign up for.
            run_command(&ctx, &target, WorkflowState::VerifyingDeployment, hook).await?;
        }
        db.finish_deployment(org, run_id, dep_id, true, None)
            .await
            .map_err(StageError::Permanent)?;
    }
    // Every path ends at completed: deploy+verify ran above, or only
    // the final hop remained.
    db.transition_run(
        org,
        run_id,
        WorkflowState::VerifyingDeployment,
        TransitionEvent::DeploymentVerified,
        "deployment-handler",
    )
    .await
    .map_err(StageError::Retryable)?;
    Ok(())
}

/// Resolve the persisted strategy note against configured targets,
/// failing the run loudly when nothing matches: the plan named a
/// deployment that configuration cannot honor, which must never
/// degrade into a guess or a default.
async fn resolve_target(
    handler: &DeploymentHandler,
    db: &Db,
    org: OrganizationId,
    run_id: WorkflowRunId,
    state: WorkflowState,
    target_name: &str,
) -> std::result::Result<DeploymentTargetSpec, StageError> {
    match handler.settings.resolve(target_name) {
        Some(target) => Ok(target.clone()),
        None => {
            fail_run(db, org, run_id, state).await?;
            Err(StageError::Permanent(Error::Validation {
                field: "strategy.deployment".into(),
                message: format!(
                    "plan names deployment target {target_name:?} but configuration has: {}",
                    handler.settings.names()
                ),
            }))
        }
    }
}

/// Everything one governed command execution needs besides the target
/// and the argv itself.
struct CmdRun<'a> {
    db: &'a Db,
    handler: &'a DeploymentHandler,
    org: OrganizationId,
    run_id: WorkflowRunId,
    dep: DeploymentId,
}

/// Execute one command of a target under an operator manifest derived
/// from the target's own commands. A non-zero exit or a timeout
/// finishes the deployment row as failed, persists a bounded reason,
/// fails the run terminally and returns a permanent stage error;
/// sandbox refusals are permanent too (configuration problems never
/// heal by retrying), while process-spawn faults stay retryable.
async fn run_command(
    ctx: &CmdRun<'_>,
    target: &DeploymentTargetSpec,
    state: WorkflowState,
    command: &DeployCommand,
) -> std::result::Result<(), StageError> {
    let manifest = RoleManifest::operator_for_commands(target.command_names().map(str::to_string));
    manifest
        .validate(&ctx.handler.registry)
        .map_err(StageError::Permanent)?;
    let workspace = ctx.handler.layout.run_repo_dir(ctx.run_id);
    let caps = manifest.capability_set(workspace);
    let shell = SandboxedShell::new(&caps).with_timeout(Duration::from_secs(target.timeout_secs));
    let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
    let outcome = match shell.execute(&command.program, &args) {
        Ok(outcome) => outcome,
        // The governed shell refused: allowlist or workspace drift is
        // a config defect retries cannot fix.
        Err(e @ Error::Forbidden { .. }) | Err(e @ Error::Validation { .. }) => {
            return Err(StageError::Permanent(e));
        }
        Err(e) => return Err(StageError::Retryable(e)),
    };

    if outcome.timed_out || outcome.exit_code != Some(0) {
        let reason = bounded_reason(
            command.program.as_str(),
            outcome.exit_code,
            outcome.timed_out,
            &outcome.stderr,
        );
        ctx.db
            .finish_deployment(ctx.org, ctx.run_id, ctx.dep, false, Some(&reason))
            .await
            .map_err(StageError::Permanent)?;
        fail_run(ctx.db, ctx.org, ctx.run_id, state).await?;
        return Err(StageError::Permanent(Error::Validation {
            field: "deployment".into(),
            message: reason,
        }));
    }
    Ok(())
}

/// Advance any non-terminal state to Failed, absorbing the lost race a
/// concurrent redelivery may already have won.
async fn fail_run(
    db: &Db,
    org: OrganizationId,
    run: WorkflowRunId,
    from: WorkflowState,
) -> std::result::Result<(), StageError> {
    if let Err(e) = db
        .transition_run(org, run, from, TransitionEvent::Fail, "deployment-handler")
        .await
        && !matches!(e, Error::Conflict { .. })
    {
        return Err(StageError::Retryable(e));
    }
    Ok(())
}

/// One bounded, evidence-grade failure line: program, exit status and
/// a stderr tail. Never the whole log - events are data.
fn bounded_reason(program: &str, exit_code: Option<i32>, timed_out: bool, stderr: &str) -> String {
    const TAIL_CHARS: usize = 400;
    let mut reason = if timed_out {
        format!("{program} timed out")
    } else {
        format!("{program} failed (exit_code={exit_code:?})")
    };
    let tail = stderr.trim();
    if !tail.is_empty() {
        reason.push(':');
        if tail.chars().count() > TAIL_CHARS {
            let skip = tail
                .char_indices()
                .rev()
                .nth(TAIL_CHARS - 1)
                .map_or(0, |(i, _)| i);
            reason.push('…');
            reason.push_str(tail[skip..].trim_start());
        } else {
            reason.push(' ');
            reason.push_str(tail);
        }
    }
    reason
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn settings_resolve_by_exact_trimmed_name() {
        let settings = DeploymentSettings::new(vec![DeploymentTargetSpec {
            name: "staging".into(),
            command: DeployCommand::from_argv(&["heph-deploy".into()]).expect("argv"),
            verify: vec![DeployCommand::from_argv(&["heph-probe".into()]).expect("argv")],
            timeout_secs: 600,
        }]);
        assert!(settings.resolve("staging").is_some());
        assert!(settings.resolve("  staging  ").is_some(), "trimmed match");
        assert!(
            settings.resolve("staging via CD").is_none(),
            "no prose parsing"
        );
        assert_eq!(settings.names(), "staging");
        assert_eq!(DeploymentSettings::default().names(), "none configured");
    }

    #[test]
    fn operator_allowlist_covers_every_target_command() {
        let target = DeploymentTargetSpec {
            name: "staging".into(),
            command: DeployCommand::from_argv(&["heph-deploy".into(), "--fast".into()])
                .expect("argv"),
            verify: vec![
                DeployCommand::from_argv(&["heph-probe".into()]).expect("argv"),
                DeployCommand::from_argv(&["heph-smoke".into()]).expect("argv"),
            ],
            timeout_secs: 60,
        };
        let manifest =
            RoleManifest::operator_for_commands(target.command_names().map(str::to_string));
        let reg = hephaestus_tools::registry::ToolRegistry::with_builtins();
        manifest
            .validate(&reg)
            .unwrap_or_else(|e| panic!("manifest must validate: {e}"));
        for name in ["heph-deploy", "heph-probe", "heph-smoke"] {
            assert!(
                manifest
                    .capability_set("/tmp/ws".into())
                    .command_allowed(name)
            );
        }
        assert!(
            !manifest
                .capability_set("/tmp/ws".into())
                .command_allowed("curl")
        );
    }

    #[test]
    fn failure_reasons_are_bounded_and_factual() {
        let r = bounded_reason("heph-deploy", Some(3), false, "boom\n");
        assert_eq!(r, "heph-deploy failed (exit_code=Some(3)): boom");

        let t = bounded_reason("heph-deploy", None, true, "");
        assert_eq!(t, "heph-deploy timed out");

        let long = "x".repeat(1000);
        let bounded = bounded_reason("p", Some(1), false, &long);
        assert!(bounded.chars().count() < 450, "{}", bounded.chars().count());
        assert!(bounded.starts_with("p failed (exit_code=Some(1)):"));
    }
}
