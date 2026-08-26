//! The `hephaestus-worker` binary.
//!
//! Wires every pipeline stage handler into the durable worker loop
//! from layered configuration: repository analysis, requirement
//! extraction and plan generation, governed implementation and repair,
//! deterministic verification, automated review, and artifact builds.
//! Model-backed stages are served only when the configuration names a
//! provider; a worker serving deterministic queues alone never holds
//! model credentials at all (fail closed, validated in
//! `hephaestus-config`).
//!
//! Division of labor with `hephaestus-server` (ADR-009/ADR-010): the
//! server owns schema evolution and the HTTP surface; this process owns
//! job processing. The worker performs no migrations - starting it
//! against an unmigrated database fails loudly on first claims rather
//! than racing another process through migrations.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hephaestus_agent::provider::{OpenAiCompatProvider, ProviderConfig};
use hephaestus_agent::session::{AuditLogSink, DecisionSink};
use hephaestus_config::{Config, MODEL_BACKED_QUEUES};
use hephaestus_core::Error;
use hephaestus_db::Db;
use hephaestus_engine::worker::{HandlerRegistry, Worker};
use hephaestus_engine::{
    AnalysisHandler, BuildHandler, DeployCommand, DeploymentHandler, DeploymentSettings,
    DeploymentTargetSpec, ExecutionHandler, ExtractionHandler, PlanningHandler, RepairHandler,
    ReviewHandler, RunVerificationHandler, SessionDeps, WorkspaceLayout,
};
use hephaestus_telemetry::{init as init_telemetry, shutdown_signal};

fn main() {
    // Config failures must print cleanly even before telemetry exists.
    match run() {
        Ok(()) => {}
        Err(err) => {
            eprintln!("fatal: {err}");
            tracing::error!(error = %err, "worker terminated");
            std::process::exit(1);
        }
    }
}

/// Configuration file path: --config flag wins over HEPHAESTUS_CONFIG.
fn config_path() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--config") {
        return args.get(pos + 1).map(PathBuf::from);
    }
    std::env::var("HEPHAESTUS_CONFIG").ok().map(PathBuf::from)
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Convert configured deployment targets into the engine's specs.
/// Config validation ran at load; this only re-shapes argv vectors.
fn deployment_settings(cfg: &Config) -> hephaestus_core::Result<DeploymentSettings> {
    let mut specs = Vec::with_capacity(cfg.deployment.targets.len());
    for target in &cfg.deployment.targets {
        specs.push(DeploymentTargetSpec {
            name: target.name.clone(),
            command: DeployCommand::from_argv(&target.command)?,
            verify: target
                .verify
                .iter()
                .map(|hook| DeployCommand::from_argv(hook))
                .collect::<hephaestus_core::Result<Vec<_>>>()?,
            timeout_secs: target.timeout_secs,
        });
    }
    Ok(DeploymentSettings::new(specs))
}

fn run() -> hephaestus_core::Result<()> {
    let cfg = Config::load(config_path().as_deref(), env_var)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Storage(Box::new(e)))?;

    runtime.block_on(async move {
        // Telemetry initializes as the first step inside the runtime,
        // before any component logs (ADR-014); its exporter runs on the
        // SDK's own background thread.
        let telemetry = init_telemetry(&cfg.telemetry)?;
        tracing::info!(
            environment = ?cfg.environment,
            database = %cfg.database.redacted_url(),
            "configuration loaded"
        );

        let db = Db::connect_with_max(&cfg.database.url, cfg.database.max_connections).await?;
        let result = serve_jobs(db, &cfg).await;
        // Flush batched spans before exit (no-op without OTLP export).
        telemetry.shutdown();
        result
    })
}

/// Assemble the handler registry from configuration and run the worker
/// loop until shutdown is requested. In-flight jobs drain gracefully;
/// leases keep them owned while draining.
async fn serve_jobs(db: Db, cfg: &Config) -> hephaestus_core::Result<()> {
    let sink: Arc<dyn DecisionSink> = Arc::new(AuditLogSink::new(db.clone()));
    let layout = WorkspaceLayout::new(cfg.storage.root.clone());

    let queues: Vec<String> = cfg
        .worker
        .queues
        .iter()
        .map(|q| q.trim().to_string())
        .collect();
    let needs_model = queues
        .iter()
        .any(|q| MODEL_BACKED_QUEUES.contains(&q.as_str()));

    // Target configuration travels with every builder: routing a
    // built change set is part of the build stage, whether or not
    // THIS process also executes deployments.
    let deployments = Arc::new(deployment_settings(cfg)?);

    // Validation guarantees provider settings whenever needs_model; a
    // deterministic-only worker intentionally builds no sessions.
    let deps = if needs_model {
        let provider = OpenAiCompatProvider::new(ProviderConfig {
            base_url: cfg.model.base_url.clone(),
            api_key: cfg.model.api_key.clone(),
            model: cfg.model.model.clone(),
            timeout: Duration::from_secs(cfg.model.timeout_secs),
            max_retries: cfg.model.max_retries,
        })?;
        tracing::info!(
            base_url = %cfg.model.base_url,
            model = %cfg.model.model,
            "model provider configured"
        );
        Some(SessionDeps::new(
            Arc::new(provider),
            Arc::clone(&sink),
            cfg.model.model.clone(),
        ))
    } else {
        None
    };

    let builds_with_targets = if deployments.targets.is_empty() {
        BuildHandler::new(layout.clone())
    } else {
        tracing::info!(
            targets = %deployments.names(),
            "deployment targets configured"
        );
        BuildHandler::new(layout.clone()).with_deployments(Arc::clone(&deployments))
    };
    let mut registry = HandlerRegistry::new()
        .with_analyze(Arc::new(AnalysisHandler::new(layout.clone())))
        .with_run_verification(Arc::new(RunVerificationHandler::new(layout.clone())))
        .with_build_artifact(Arc::new(builds_with_targets));
    if queues.iter().any(|q| q == "deployment") {
        registry = registry.with_deploy_change_set(Arc::new(DeploymentHandler::new(
            layout.clone(),
            Arc::clone(&deployments),
        )));
    }
    if let Some(deps) = deps {
        registry = registry
            .with_extract_requirements(Arc::new(ExtractionHandler::new(
                deps.clone(),
                layout.clone(),
            )))
            .with_generate_plan(Arc::new(PlanningHandler::new(deps.clone(), layout.clone())))
            .with_execute_step(Arc::new(ExecutionHandler::new(
                deps.clone(),
                layout.clone(),
            )))
            .with_repair_execution(Arc::new(RepairHandler::new(deps.clone(), layout.clone())))
            .with_run_review(Arc::new(ReviewHandler::new(deps, layout)));
    }

    let id = if cfg.worker.id.trim().is_empty() {
        format!("worker-{}", uuid::Uuid::now_v7().simple())
    } else {
        cfg.worker.id.trim().to_string()
    };
    let loop_config = hephaestus_engine::worker::WorkerConfig {
        id,
        queues,
        lease_ttl_secs: i64::try_from(cfg.worker.lease_ttl_secs)
            .map_err(|_| Error::Config("worker.lease_ttl_secs out of range".into()))?,
        poll_interval: Duration::from_millis(cfg.worker.poll_interval_ms),
        concurrency: cfg.worker.concurrency as usize,
    };

    let worker = Arc::new(Worker::new(db, registry, loop_config));
    let shutdown = tokio_util::sync::CancellationToken::new();
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            // Ctrl-C and SIGTERM drain identically (ADR-014): process
            // managers stop workers with SIGTERM, never Ctrl-C.
            shutdown_signal().await;
            tracing::warn!("shutdown signal received; draining jobs");
            shutdown.cancel();
        });
    }
    worker.run(shutdown).await;
    Ok(())
}
