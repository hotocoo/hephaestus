//! The `hephaestus-server` binary.
//!
//! Loads layered configuration, initializes telemetry, applies
//! versioned migrations on the configured database, and serves the
//! HTTP API until shutdown is requested. Job processing stays with
//! workers (ADR-009); this process holds no model-provider secrets.

use std::path::PathBuf;

use hephaestus_api::{AppState, AuthPolicy, router};
use hephaestus_config::Config;
use hephaestus_core::Error;
use hephaestus_db::Db;

fn main() {
    // Config failures must print cleanly even before telemetry exists.
    match run() {
        Ok(()) => {}
        Err(err) => {
            eprintln!("fatal: {err}");
            tracing::error!(error = %err, "server terminated");
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

fn run() -> hephaestus_core::Result<()> {
    let cfg = Config::load(config_path().as_deref(), env_var)?;

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            cfg.telemetry.log_level.clone(),
        ))
        .init();
    tracing::info!(
        environment = ?cfg.environment,
        database = %cfg.database.redacted_url(),
        "configuration loaded"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(if cfg.server.workers == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            cfg.server.workers
        })
        .build()
        .map_err(|e| Error::Storage(Box::new(e)))?;

    runtime.block_on(async move {
        let db = Db::connect(&cfg.database.url).await?;
        db.migrate().await?;
        tracing::info!("migrations applied");

        let auth = AuthPolicy::new(cfg.auth.disabled, &cfg.auth.keys);
        let state = AppState::new(db, auth, cfg.server.max_body_bytes);
        let app = router(state);

        let addr = format!("{}:{}", cfg.server.bind_addr, cfg.server.port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::Storage(Box::new(e)))?;
        tracing::info!(%addr, "serving");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| Error::Storage(Box::new(e)))
    })
}

/// Resolve when the operator asks the process to stop.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::warn!("shutdown signal received; draining connections");
}
