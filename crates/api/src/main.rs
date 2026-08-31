//! The hephaestus-server binary.
//!
//! Loads layered configuration, initializes telemetry (local logging
//! always, OTLP export when configured - ADR-014), optionally mounts
//! the built dashboard from disk, applies versioned migrations on the
//! configured database, and serves the HTTP API until Ctrl-C or
//! SIGTERM requests shutdown. Job processing stays with workers
//! (ADR-009); this process holds no model-provider secrets.

use std::path::PathBuf;

use hephaestus_api::{AppState, AuthPolicy, WebSite, router};
use hephaestus_config::Config;
use hephaestus_core::Error;
use hephaestus_db::Db;
use hephaestus_telemetry::{init as init_telemetry, shutdown_signal};

fn main() {
    // Config failures must print cleanly even before telemetry exists.
    match run() {
        Ok(()) => {}
        Err(err) => {
            eprintln!("fatal: {err}");
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

    // Telemetry initializes as the first step inside the runtime,
    // before any component logs (ADR-014); its exporter runs on the
    // SDK's own background thread.
    runtime.block_on(async move {
        let telemetry = init_telemetry(&cfg.telemetry)?;
        tracing::info!(
            environment = ?cfg.environment,
            database = %cfg.database.redacted_url(),
            web_serving = cfg.web.dist_dir.is_some(),
            storage_root = %cfg.storage.root.display(),
            "configuration loaded"
        );

        let db = Db::connect(&cfg.database.url).await?;
        db.migrate().await?;
        tracing::info!("migrations applied");

        // Load the dashboard before binding: a broken bundle is a
        // startup failure, not a first-request surprise.
        let mut state = AppState::new(
            db.clone(),
            AuthPolicy::new(cfg.auth.disabled, &cfg.auth.keys),
            cfg.server.max_body_bytes,
        )
        .with_request_timeout(cfg.server.request_timeout_secs)
        .with_storage_root(cfg.storage.root.clone());
        if let Some(dist_dir) = cfg.web.dist_dir.clone() {
            let site = WebSite::load(&dist_dir, cfg.web.runtime_config.as_ref())?;
            tracing::info!(dir = %dist_dir.display(), "serving dashboard");
            state = state.with_web_site(site);
        }

        let app = router(state);

        let addr = format!("{}:{}", cfg.server.bind_addr, cfg.server.port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::Storage(Box::new(e)))?;
        tracing::info!(%addr, "serving");
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| Error::Storage(Box::new(e)));
        // Flush batched spans before exit (no-op without OTLP export).
        telemetry.shutdown();
        served
    })
}
