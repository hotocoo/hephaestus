//! # hephaestus-config
//!
//! Layered configuration with fail-closed validation.
//!
//! Precedence (lowest to highest):
//! 1. built-in safe defaults
//! 2. configuration file (TOML; path via HEPHAESTUS_CONFIG or --config)
//! 3. environment variables (HEPHAESTUS_ prefix)
//!
//! Configuration is validated at load time. Invalid configuration is a
//! hard error: the process refuses to start rather than falling back
//! to unsafe behavior.
//!
//! Secrets never appear in Debug output of this structure.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use hephaestus_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// Top-level Hephaestus configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Server settings (API bind address, ports).
    pub server: ServerConfig,
    /// Database settings.
    pub database: DatabaseConfig,
    /// Artifact/object storage settings.
    pub storage: StorageConfig,
    /// Telemetry settings.
    pub telemetry: TelemetryConfig,
    /// Sandbox resource limits for untrusted execution.
    pub sandbox: SandboxLimits,
    /// Authentication settings.
    pub auth: AuthConfig,
    /// Model provider settings (worker-side; the API process holds no
    /// provider credentials per ADR-009).
    pub model: ModelConfig,
    /// Worker loop settings.
    pub worker: WorkerSettings,
    /// Deployment targets (ADR-013). Empty means no executor exists and
    /// runs demanding deployment fail loudly, exactly as before.
    pub deployment: DeploymentConfig,
    /// Dashboard serving settings (ADR-014). Unconfigured means the
    /// server exposes only the API, exactly as before.
    pub web: WebConfig,
    /// Environment name: development | test | staging | production.
    pub environment: Environment,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            storage: StorageConfig::default(),
            telemetry: TelemetryConfig::default(),
            sandbox: SandboxLimits::default(),
            auth: AuthConfig::default(),
            model: ModelConfig::default(),
            worker: WorkerSettings::default(),
            deployment: DeploymentConfig::default(),
            web: WebConfig::default(),
            environment: Environment::Development,
        }
    }
}

/// Deployment environment. Controls fail-closed strictness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    /// Local development.
    Development,
    /// Automated tests.
    Test,
    /// Staging environment.
    Staging,
    /// Production: strictest validation.
    Production,
}

impl Environment {
    /// Whether this is the production environment.
    pub fn is_production(self) -> bool {
        self == Environment::Production
    }
}

/// HTTP API server settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Bind address for the API server.
    pub bind_addr: String,
    /// Bind port for the API server.
    pub port: u16,
    /// Worker thread hint (0 = tokio default).
    pub workers: usize,
    /// Global request body size limit in bytes.
    pub max_body_bytes: usize,
    /// Wall-clock cap for one request, in seconds (ADR-014): hung
    /// handlers release their worker instead of pinning it.
    pub request_timeout_secs: u64,
}

/// Inclusive bounds for server.request_timeout_secs.
pub const REQUEST_TIMEOUT_RANGE: std::ops::RangeInclusive<u64> = 1..=600;

/// Dashboard serving settings ([web], ADR-014).
///
/// Unconfigured (dist_dir absent), the API binary serves no files and
/// behaves exactly as earlier ADRs specified. Configured, it also
/// serves the built dashboard from disk so one process fronts both the
/// control plane and its human surface, same-origin by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
    /// Directory holding the built dashboard (its index.html plus
    /// assets). Absent disables static serving entirely.
    pub dist_dir: Option<PathBuf>,
    /// Runtime configuration injected into the served index.html;
    /// absent leaves the bundle's fail-closed placeholder untouched
    /// (the dashboard renders its setup screen without a token).
    pub runtime_config: Option<WebRuntimeConfig>,
}

/// Values injected into the dashboard's bootstrap global.
///
/// The token is a bearer secret: redacted from Debug output, and
/// serialized configuration must never be logged - mirroring ApiKey
/// handling.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebRuntimeConfig {
    /// Control-plane base URL without trailing slash. Empty means
    /// same-origin (the default when hephaestus-server serves the
    /// dashboard itself).
    pub base_url: String,
    /// Pre-provisioned API key handed to the dashboard. Empty keeps
    /// the app on its setup screen (fail closed).
    pub token: String,
    /// Live-view polling interval in seconds.
    pub poll_seconds: u64,
}

impl Default for WebRuntimeConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            token: String::new(),
            // Mirrors the dashboard's own default polling interval so
            // an operator who sets only the token gets documented
            // behavior, not an accidental 0-second hot loop.
            poll_seconds: 10,
        }
    }
}

impl fmt::Debug for WebRuntimeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebRuntimeConfig")
            .field("base_url", &self.base_url)
            .field("token", &"[redacted]")
            .field("poll_seconds", &self.poll_seconds)
            .finish()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1".into(),
            port: 7300,
            workers: 0,
            max_body_bytes: 8 * 1024 * 1024,
            request_timeout_secs: 30,
        }
    }
}

/// PostgreSQL connection settings.
///
/// The URL may embed credentials; it is never logged verbatim -
/// use [`DatabaseConfig::redacted_url`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DatabaseConfig {
    /// Full connection URL, e.g. postgres://user:pass@host/db.
    pub url: String,
    /// Maximum pooled connections.
    pub max_connections: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        // Safe generic local default; documented and overrideable.
        Self {
            url: "postgres://localhost/hephaestus_dev".into(),
            max_connections: 10,
        }
    }
}

impl DatabaseConfig {
    /// Rendered form safe for logs: credentials stripped.
    pub fn redacted_url(&self) -> String {
        redact_url(&self.url)
    }
}

/// Strip userinfo from a URL for logging.
pub fn redact_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        // Unparseable URLs are not rendered at all.
        Err(_) => "<unparseable-url>".into(),
    }
}

/// Local artifact storage settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StorageConfig {
    /// Root directory for artifacts and evidence blobs.
    pub root: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from(".hephaestus/data"),
        }
    }
}

/// Telemetry export settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TelemetryConfig {
    /// Log verbosity: error|warn|info|debug|trace.
    pub log_level: String,
    /// Optional OTLP endpoint; absent disables export.
    pub otlp_endpoint: Option<String>,
    /// Service name used in traces/metrics.
    pub service_name: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            otlp_endpoint: None,
            service_name: "hephaestus".into(),
        }
    }
}

/// Resource limits applied to every sandboxed execution.
///
/// These are enforced by the execution layer; config declares them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SandboxLimits {
    /// Wall-clock timeout per command in seconds.
    pub command_timeout_secs: u64,
    /// Maximum concurrent sandboxed executions per worker.
    pub max_concurrency: u32,
    /// Cap on captured stdout/stderr per command in bytes.
    pub max_output_bytes: usize,
    /// Network egress from sandboxes (default: disabled).
    pub allow_network: bool,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            command_timeout_secs: 600,
            max_concurrency: 4,
            max_output_bytes: 8 * 1024 * 1024,
            allow_network: false,
        }
    }
}

/// One pre-provisioned API key bound to a tenant.
///
/// Keys are created out-of-band and handed to operators; there is no
/// token-issuance endpoint. Every request authenticated by a key is
/// scoped to its organization and attributed to its principal.
///
/// The token is a bearer secret: it never appears in Debug output.
/// `Serialize` output *does* contain it - serialized config must
/// never be logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKey {
    /// Bearer token presented as `Authorization: Bearer <token>`.
    pub token: String,
    /// Organization every request authenticated by this key is scoped to.
    pub organization_id: uuid::Uuid,
    /// Principal recorded on decisions made through this key.
    pub principal: String,
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKey")
            .field("token", &"[redacted]")
            .field("organization_id", &self.organization_id)
            .field("principal", &self.principal)
            .finish()
    }
}

/// Authentication settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthConfig {
    /// Disable authentication entirely. NEVER valid outside development
    /// and test environments; validation rejects it elsewhere.
    pub disabled: bool,
    /// Bearer token lifetime in seconds.
    pub token_ttl_secs: u64,
    /// Pre-provisioned API keys. Empty is valid only when auth is
    /// disabled or the environment is development/test; production
    /// validation demands at least one key otherwise.
    pub keys: Vec<ApiKey>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            token_ttl_secs: 3600 * 12,
            keys: Vec::new(),
        }
    }
}

/// Queue names a worker process can serve.
///
/// Mirrors `hephaestus_engine::jobs::Queue::as_str`; the worker crate
/// pins the two lists together with a test so they cannot drift. Every
/// engine queue is listed; serving one still demands its own
/// preconditions - model-backed queues require provider settings and
/// `deployment` requires at least one configured target (ADR-013).
/// Configuring capacity for a queue whose preconditions fail would be
/// simulation by another name, so validation refuses it outright.
pub const WORKER_QUEUE_NAMES: [&str; 7] = [
    "analysis",
    "planning",
    "implementation",
    "verification",
    "review",
    "build",
    "deployment",
];

/// Queues whose handlers run governed model sessions.
pub const MODEL_BACKED_QUEUES: [&str; 3] = ["planning", "implementation", "review"];

/// Model provider settings.
///
/// The API server never holds these credentials (ADR-009); only the
/// worker binary builds providers. The API key is a bearer secret:
/// it is redacted from Debug output, and serialized config must never
/// be logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelConfig {
    /// Base URL of an OpenAI-compatible endpoint, e.g.
    /// https://gateway.internal/v1. Empty means unconfigured.
    pub base_url: String,
    /// Bearer credential sent as Authorization header. Absent is legal
    /// only against gateways that do their own network-level access
    /// control; production demands it.
    pub api_key: Option<String>,
    /// Default model identifier served by the endpoint.
    pub model: String,
    /// Per-request wall-clock timeout in seconds.
    pub timeout_secs: u64,
    /// Extra attempts after the first on retryable failures.
    pub max_retries: u32,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: None,
            model: String::new(),
            timeout_secs: 120,
            max_retries: 2,
        }
    }
}

impl fmt::Debug for ModelConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("model", &self.model)
            .field("timeout_secs", &self.timeout_secs)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

/// Worker loop settings ([worker]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorkerSettings {
    /// Stable lease-owner identity. Empty means generate one at start.
    pub id: String,
    /// Queues this worker serves. Every name must be a known queue;
    /// duplicates and unknown names fail validation (fail closed).
    pub queues: Vec<String>,
    /// Lease duration in seconds. Heartbeats renew at ttl/3.
    pub lease_ttl_secs: u64,
    /// Idle poll interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Concurrent job slots per worker process.
    pub concurrency: u32,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        // Defaults deliberately serve ONLY deterministic queues: they
        // validate cleanly with no model credentials anywhere. Serving
        // model-backed queues is an explicit operator decision that
        // drags [model] configuration along with it (fail closed).
        Self {
            id: String::new(),
            queues: vec![
                "analysis".to_string(),
                "verification".to_string(),
                "build".to_string(),
            ],
            lease_ttl_secs: 300,
            poll_interval_ms: 500,
            concurrency: 4,
        }
    }
}

/// One configured deployment target (ADR-013).
///
/// A target is a named, deterministic command plus MANDATORY
/// post-deployment verification hooks, executed through the governed
/// sandboxed shell as argv vectors inside the run workspace. Programs
/// must be bare executable names - the same allowlist rule every other
/// shell capability obeys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentTarget {
    /// Name plans cite in `strategy.deployment` to select this target.
    pub name: String,
    /// Deploy command: first element is the program, the rest arguments.
    pub command: Vec<String>,
    /// Post-deployment verification hooks, each an argv vector. Every
    /// hook must exit zero for the deployment to verify; empty is a
    /// configuration error because an unchecked deployment is a
    /// simulation risk, not a feature.
    pub verify: Vec<Vec<String>>,
    /// Wall-clock cap for the deploy command and for each hook.
    pub timeout_secs: u64,
}

/// Deployment target settings (`[[deployment.targets]]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentConfig {
    /// Named targets. Empty means no executor exists: the `deployment`
    /// queue becomes unservable and runs demanding deployment fail
    /// loudly, exactly as ADR-008 specified before executors landed.
    pub targets: Vec<DeploymentTarget>,
}

/// Bounds for per-target wall-clock caps (inclusive).
pub const DEPLOY_TIMEOUT_RANGE: std::ops::RangeInclusive<u64> = 10..=3600;

/// Timeout used when a target omits `timeout_secs`.
pub const DEFAULT_DEPLOY_TIMEOUT_SECS: u64 = 600;

impl DeploymentConfig {
    /// The named target, if configured.
    pub fn find(&self, name: &str) -> Option<&DeploymentTarget> {
        self.targets.iter().find(|t| t.name == name)
    }
}

impl Config {
    /// Load layered configuration.
    ///
    /// `file_path` may be None (defaults + env only). Environment
    /// overrides use HEPHAESTUS_-prefixed variables for deployment-critical
    /// values:
    ///
    /// * HEPHAESTUS_ENVIRONMENT
    /// * HEPHAESTUS_DATABASE_URL
    /// * HEPHAESTUS_BIND_ADDR
    /// * HEPHAESTUS_PORT
    /// * HEPHAESTUS_STORAGE_ROOT
    /// * HEPHAESTUS_OTLP_ENDPOINT
    /// * HEPHAESTUS_LOG_LEVEL
    pub fn load(file_path: Option<&Path>, env: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let mut cfg = Config::default();

        if let Some(path) = file_path {
            let raw = std::fs::read_to_string(path).map_err(|e| {
                hephaestus_core::Error::Config(format!(
                    "cannot read config file {}: {e}",
                    path.display()
                ))
            })?;
            let parsed: ConfigFile = toml::from_str(&raw)
                .map_err(|e| hephaestus_core::Error::Config(format!("invalid TOML config: {e}")))?;
            cfg.apply_file(parsed);
        }

        cfg.apply_env(&env);
        cfg.validate()?;
        Ok(cfg)
    }

    fn apply_file(&mut self, f: ConfigFile) {
        if let Some(server) = f.server {
            if let Some(v) = server.bind_addr {
                self.server.bind_addr = v;
            }
            if let Some(v) = server.port {
                self.server.port = v;
            }
            if let Some(v) = server.workers {
                self.server.workers = v;
            }
            if let Some(v) = server.max_body_bytes {
                self.server.max_body_bytes = v;
            }
            if let Some(v) = server.request_timeout_secs {
                self.server.request_timeout_secs = v;
            }
        }
        if let Some(db) = f.database {
            if let Some(v) = db.url {
                self.database.url = v;
            }
            if let Some(v) = db.max_connections {
                self.database.max_connections = v;
            }
        }
        if let Some(storage) = f.storage
            && let Some(v) = storage.root
        {
            self.storage.root = PathBuf::from(v);
        }
        if let Some(t) = f.telemetry {
            if let Some(v) = t.log_level {
                self.telemetry.log_level = v;
            }
            if let Some(v) = t.otlp_endpoint {
                self.telemetry.otlp_endpoint = Some(v);
            }
            if let Some(v) = t.service_name {
                self.telemetry.service_name = v;
            }
        }
        if let Some(s) = f.sandbox {
            if let Some(v) = s.command_timeout_secs {
                self.sandbox.command_timeout_secs = v;
            }
            if let Some(v) = s.max_concurrency {
                self.sandbox.max_concurrency = v;
            }
            if let Some(v) = s.max_output_bytes {
                self.sandbox.max_output_bytes = v;
            }
            if let Some(v) = s.allow_network {
                self.sandbox.allow_network = v;
            }
        }
        if let Some(a) = f.auth {
            if let Some(v) = a.disabled {
                self.auth.disabled = v;
            }
            if let Some(v) = a.token_ttl_secs {
                self.auth.token_ttl_secs = v;
            }
            // The file section defines the complete key set.
            if let Some(keys) = a.keys {
                self.auth.keys = keys
                    .into_iter()
                    .map(|k| ApiKey {
                        token: k.token,
                        organization_id: k.organization_id,
                        principal: k.principal,
                    })
                    .collect();
            }
        }
        if let Some(m) = f.model {
            if let Some(v) = m.base_url {
                self.model.base_url = v;
            }
            if let Some(v) = m.api_key {
                self.model.api_key = if v.trim().is_empty() { None } else { Some(v) };
            }
            if let Some(v) = m.model {
                self.model.model = v;
            }
            if let Some(v) = m.timeout_secs {
                self.model.timeout_secs = v;
            }
            if let Some(v) = m.max_retries {
                self.model.max_retries = v;
            }
        }
        if let Some(w) = f.worker {
            // The file section defines the complete queue set, matching
            // how [auth] keys replace rather than extend.
            if let Some(queues) = w.queues {
                self.worker.queues = queues;
            }
            if let Some(v) = w.id {
                self.worker.id = v;
            }
            if let Some(v) = w.lease_ttl_secs {
                self.worker.lease_ttl_secs = v;
            }
            if let Some(v) = w.poll_interval_ms {
                self.worker.poll_interval_ms = v;
            }
            if let Some(v) = w.concurrency {
                self.worker.concurrency = v;
            }
        }
        if let Some(d) = f.deployment
            && let Some(targets) = d.targets
        {
            // The file section defines the complete target set; targets
            // are deployment facts, not incremental overrides.
            self.deployment.targets = targets
                .into_iter()
                .map(|t| DeploymentTarget {
                    name: t.name,
                    command: t.command,
                    verify: t.verify,
                    timeout_secs: t.timeout_secs.unwrap_or(DEFAULT_DEPLOY_TIMEOUT_SECS),
                })
                .collect();
        }
        if let Some(w) = f.web {
            if let Some(v) = w.dist_dir {
                self.web.dist_dir = Some(PathBuf::from(v));
            }
            if let Some(rc) = w.runtime_config {
                let target = self
                    .web
                    .runtime_config
                    .get_or_insert_with(WebRuntimeConfig::default);
                if let Some(v) = rc.base_url {
                    target.base_url = v;
                }
                if let Some(v) = rc.token {
                    target.token = v;
                }
                if let Some(v) = rc.poll_seconds {
                    target.poll_seconds = v;
                }
            }
        }
        if let Some(e) = f.environment {
            self.environment = e;
        }
    }

    fn apply_env(&mut self, env: &impl Fn(&str) -> Option<String>) {
        if let Some(v) = env("HEPHAESTUS_ENVIRONMENT") {
            self.environment = parse_environment(&v);
        }
        if let Some(v) = env("HEPHAESTUS_DATABASE_URL") {
            self.database.url = v;
        }
        if let Some(v) = env("HEPHAESTUS_BIND_ADDR") {
            self.server.bind_addr = v;
        }
        if let Some(v) = env("HEPHAESTUS_PORT") {
            self.server.port = parse_port(&v);
        }
        if let Some(v) = env("HEPHAESTUS_STORAGE_ROOT") {
            self.storage.root = PathBuf::from(v);
        }
        if let Some(v) = env("HEPHAESTUS_OTLP_ENDPOINT")
            && !v.is_empty()
        {
            self.telemetry.otlp_endpoint = Some(v);
        }
        if let Some(v) = env("HEPHAESTUS_LOG_LEVEL") {
            self.telemetry.log_level = v;
        }
        if let Some(v) = env("HEPHAESTUS_REQUEST_TIMEOUT_SECS") {
            self.server.request_timeout_secs = parse_u64_env("HEPHAESTUS_REQUEST_TIMEOUT_SECS", &v);
        }
        if let Some(v) = env("HEPHAESTUS_WEB_DIST_DIR") {
            self.web.dist_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = env("HEPHAESTUS_WEB_TOKEN")
            && !v.is_empty()
        {
            self.web
                .runtime_config
                .get_or_insert_with(WebRuntimeConfig::default)
                .token = v;
        }
        if let Some(v) = env("HEPHAESTUS_AUTH_KEYS") {
            self.auth.keys = parse_keys_env(&v);
        }
        if let Some(v) = env("HEPHAESTUS_MODEL_BASE_URL") {
            self.model.base_url = v;
        }
        if let Some(v) = env("HEPHAESTUS_MODEL_API_KEY")
            && !v.is_empty()
        {
            self.model.api_key = Some(v);
        }
        if let Some(v) = env("HEPHAESTUS_MODEL_NAME") {
            self.model.model = v;
        }
        if let Some(v) = env("HEPHAESTUS_WORKER_ID") {
            self.worker.id = v;
        }
        if let Some(v) = env("HEPHAESTUS_WORKER_QUEUES") {
            self.worker.queues = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
    }

    /// Validate the assembled configuration. Fails closed.
    pub fn validate(&self) -> Result<()> {
        let parsed = url::Url::parse(&self.database.url).map_err(|_| {
            hephaestus_core::Error::Config("database.url is not a valid URL".into())
        })?;
        let scheme = parsed.scheme();
        if scheme != "postgres" && scheme != "postgresql" {
            return Err(hephaestus_core::Error::Config(format!(
                "database.url must use postgres scheme, got {scheme:?}"
            )));
        }
        if self.database.max_connections == 0 || self.database.max_connections > 1000 {
            return Err(hephaestus_core::Error::Config(
                "database.max_connections must be within 1..=1000".into(),
            ));
        }
        if self.server.bind_addr.trim().is_empty() {
            return Err(hephaestus_core::Error::Config(
                "server.bind_addr must not be empty".into(),
            ));
        }
        if !REQUEST_TIMEOUT_RANGE.contains(&self.server.request_timeout_secs) {
            return Err(hephaestus_core::Error::Config(format!(
                "server.request_timeout_secs must be within {}..={}",
                REQUEST_TIMEOUT_RANGE.start(),
                REQUEST_TIMEOUT_RANGE.end()
            )));
        }
        // API keys must be attributable, non-trivial and unique; tokens
        // double as bearer secrets so short values would be guessable.
        let mut seen_tokens = HashSet::new();
        for key in &self.auth.keys {
            if key.token.trim().chars().count() < 16 {
                return Err(Error::Config(
                    "auth.keys token must be at least 16 characters".into(),
                ));
            }
            if key.principal.trim().is_empty() {
                return Err(Error::Config(
                    "auth.keys principal must not be empty".into(),
                ));
            }
            if !seen_tokens.insert(key.token.clone()) {
                return Err(Error::Config("auth.keys contains duplicate tokens".into()));
            }
        }
        matches!(
            self.telemetry.log_level.to_lowercase().as_str(),
            "error" | "warn" | "info" | "debug" | "trace"
        )
        .then_some(())
        .ok_or_else(|| {
            hephaestus_core::Error::Config(
                "telemetry.log_level must be one of error|warn|info|debug|trace".into(),
            )
        })?;
        // Worker settings fail closed: an operator who typos a queue
        // name must not discover it from silent idleness hours later.
        if self.worker.queues.is_empty() {
            return Err(hephaestus_core::Error::Config(
                "worker.queues must not be empty".into(),
            ));
        }
        let mut seen_queues = HashSet::new();
        for q in &self.worker.queues {
            let name = q.trim();
            if !WORKER_QUEUE_NAMES.contains(&name) {
                return Err(hephaestus_core::Error::Config(format!(
                    "worker.queues contains unknown queue {name:?}; valid queues are {}",
                    WORKER_QUEUE_NAMES.join(", ")
                )));
            }
            if !seen_queues.insert(name) {
                return Err(hephaestus_core::Error::Config(format!(
                    "worker.queues contains duplicate entry {name:?}"
                )));
            }
        }
        if !(10..=3600).contains(&self.worker.lease_ttl_secs) {
            return Err(hephaestus_core::Error::Config(
                "worker.lease_ttl_secs must be within 10..=3600 (heartbeats renew at ttl/3)".into(),
            ));
        }
        if self.worker.poll_interval_ms < 10 {
            return Err(hephaestus_core::Error::Config(
                "worker.poll_interval_ms must be at least 10".into(),
            ));
        }
        if !(1..=64).contains(&self.worker.concurrency) {
            return Err(hephaestus_core::Error::Config(
                "worker.concurrency must be within 1..=64".into(),
            ));
        }
        self.validate_deployment_targets()?;
        // Model-backed queues demand a configured provider; serving
        // them without one would retry every job into dead-letter.
        let needs_model = self
            .worker
            .queues
            .iter()
            .any(|q| MODEL_BACKED_QUEUES.contains(&q.trim()));
        if needs_model {
            if !(self.model.base_url.starts_with("http://")
                || self.model.base_url.starts_with("https://"))
            {
                return Err(hephaestus_core::Error::Config(
                    "model.base_url must be an http(s) URL when any of planning, implementation                      or review queues are served"
                        .into(),
                ));
            }
            if self.model.model.trim().is_empty() {
                return Err(hephaestus_core::Error::Config(
                    "model.model must be set when any of planning, implementation or review                      queues are served"
                        .into(),
                ));
            }
        }
        // The deployment queue demands a configured executor: capacity
        // for a queue that can never receive jobs would be simulation
        // by another name (ADR-013).
        let serves_deployment = self.worker.queues.iter().any(|q| q.trim() == "deployment");
        if serves_deployment && self.deployment.targets.is_empty() {
            return Err(hephaestus_core::Error::Config(
                "worker.queues: serving 'deployment' requires at least one \
                 [deployment.targets] entry"
                    .into(),
            ));
        }
        if self.environment.is_production() {
            if self.auth.disabled {
                return Err(hephaestus_core::Error::Config(
                    "auth.disabled=true is forbidden in production".into(),
                ));
            }
            if self.sandbox.allow_network {
                return Err(hephaestus_core::Error::Config(
                    "sandbox network egress is forbidden in production by default policy".into(),
                ));
            }
            if parsed.host_str().is_none_or(|h| h == "localhost") {
                return Err(hephaestus_core::Error::Config(
                    "production database must not point at localhost".into(),
                ));
            }
            if self.auth.keys.is_empty() {
                return Err(hephaestus_core::Error::Config(
                    "production requires at least one auth.keys entry when auth is enabled".into(),
                ));
            }
            let needs_model = self
                .worker
                .queues
                .iter()
                .any(|q| MODEL_BACKED_QUEUES.contains(&q.trim()));
            if needs_model && self.model.api_key.is_none() {
                return Err(hephaestus_core::Error::Config(
                    "production requires model.api_key when any of planning, implementation or \
                     review queues are served"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    /// Fail closed on malformed deployment targets: duplicate names,
    /// non-bare program names, empty argv elements and missing or
    /// unbounded timeouts are startup errors, not deploy-time surprises.
    fn validate_deployment_targets(&self) -> Result<()> {
        let mut seen = HashSet::new();
        for target in &self.deployment.targets {
            let name = target.name.trim();
            if name.is_empty() {
                return Err(hephaestus_core::Error::Config(
                    "deployment.targets name must not be empty".into(),
                ));
            }
            if !seen.insert(name.to_string()) {
                return Err(hephaestus_core::Error::Config(format!(
                    "deployment.targets contains duplicate name {name:?}"
                )));
            }
            validate_target_argv(name, "command", &target.command)?;
            if target.verify.is_empty() {
                return Err(hephaestus_core::Error::Config(format!(
                    "deployment.targets[{name}].verify must hold at least one hook; an \
                     unchecked deployment is a simulation risk"
                )));
            }
            for (idx, hook) in target.verify.iter().enumerate() {
                validate_target_argv(name, &format!("verify[{idx}]"), hook)?;
            }
            if !DEPLOY_TIMEOUT_RANGE.contains(&target.timeout_secs) {
                return Err(hephaestus_core::Error::Config(format!(
                    "deployment.targets[{name}].timeout_secs must be within \
                     {}..={}",
                    DEPLOY_TIMEOUT_RANGE.start(),
                    DEPLOY_TIMEOUT_RANGE.end()
                )));
            }
        }
        Ok(())
    }
}

/// One argv vector: non-empty overall, bare program name first, no
/// blank elements anywhere. Shared by command and hooks.
fn validate_target_argv(target: &str, what: &str, argv: &[String]) -> Result<()> {
    let Some(program) = argv.first() else {
        return Err(hephaestus_core::Error::Config(format!(
            "deployment.targets[{target}].{what} must not be empty"
        )));
    };
    let program = program.trim();
    if program.is_empty()
        || program == "."
        || program == ".."
        || program.contains('/')
        || program.contains('\\')
    {
        return Err(hephaestus_core::Error::Config(format!(
            "deployment.targets[{target}].{what} program {program:?} is not a bare executable name"
        )));
    }
    for arg in argv {
        if arg.trim().is_empty() {
            return Err(hephaestus_core::Error::Config(format!(
                "deployment.targets[{target}].{what} contains a blank argument"
            )));
        }
    }
    Ok(())
}

fn parse_environment(v: &str) -> Environment {
    match v.to_lowercase().as_str() {
        "development" | "dev" => Environment::Development,
        "test" => Environment::Test,
        "staging" => Environment::Staging,
        "production" | "prod" => Environment::Production,
        _ => fatal_config("HEPHAESTUS_ENVIRONMENT", v),
    }
}

fn parse_port(v: &str) -> u16 {
    v.parse()
        .unwrap_or_else(|_| fatal_config("HEPHAESTUS_PORT", v))
}

/// Parse a numeric environment override; malformed input aborts before
/// any state exists (fail closed), matching HEPHAESTUS_PORT behavior.
fn parse_u64_env(name: &str, v: &str) -> u64 {
    v.parse().unwrap_or_else(|_| fatal_config(name, v))
}

/// Parse the HEPHAESTUS_AUTH_KEYS environment override.
///
/// Format: semicolon-separated entries of comma-separated fields,
/// `principal,organization_id,token`. Malformed input aborts before
/// any state exists (fail closed), matching HEPHAESTUS_PORT behavior.
fn parse_keys_env(raw: &str) -> Vec<ApiKey> {
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|entry| {
            let fields: Vec<&str> = entry.split(',').map(str::trim).collect();
            let [principal, org, token] = fields.as_slice() else {
                fatal_config("HEPHAESTUS_AUTH_KEYS", entry);
            };
            let organization_id = org.parse::<uuid::Uuid>().unwrap_or_else(|_| {
                fatal_config("HEPHAESTUS_AUTH_KEYS", entry);
            });
            ApiKey {
                token: (*token).to_string(),
                organization_id,
                principal: (*principal).to_string(),
            }
        })
        .collect()
}

/// Invalid env overrides abort before any state exists: fail closed.
fn fatal_config(name: &str, value: &str) -> ! {
    eprintln!("fatal: invalid value for {name}: {value:?}");
    std::process::exit(78); // EX_CONFIG
}

/// File-level shape: every section optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct ConfigFile {
    server: Option<FileServer>,
    database: Option<FileDatabase>,
    storage: Option<FileStorage>,
    telemetry: Option<FileTelemetry>,
    sandbox: Option<FileSandbox>,
    auth: Option<FileAuth>,
    model: Option<FileModel>,
    worker: Option<FileWorker>,
    deployment: Option<FileDeployment>,
    web: Option<FileWeb>,
    environment: Option<Environment>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileWeb {
    dist_dir: Option<String>,
    runtime_config: Option<FileWebRuntimeConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileWebRuntimeConfig {
    base_url: Option<String>,
    token: Option<String>,
    poll_seconds: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileServer {
    bind_addr: Option<String>,
    port: Option<u16>,
    workers: Option<usize>,
    max_body_bytes: Option<usize>,
    request_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileDatabase {
    url: Option<String>,
    max_connections: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileStorage {
    root: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileTelemetry {
    log_level: Option<String>,
    otlp_endpoint: Option<String>,
    service_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileSandbox {
    command_timeout_secs: Option<u64>,
    max_concurrency: Option<u32>,
    max_output_bytes: Option<usize>,
    allow_network: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileAuth {
    disabled: Option<bool>,
    token_ttl_secs: Option<u64>,
    keys: Option<Vec<FileApiKey>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileApiKey {
    token: String,
    organization_id: uuid::Uuid,
    principal: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileModel {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    timeout_secs: Option<u64>,
    max_retries: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileWorker {
    id: Option<String>,
    queues: Option<Vec<String>>,
    lease_ttl_secs: Option<u64>,
    poll_interval_ms: Option<u64>,
    concurrency: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileDeployment {
    targets: Option<Vec<FileDeploymentTarget>>,
}

/// Required fields stay required at file level too: a target without a
/// command or without hooks must fail to parse, not load half-formed.
/// `name` and `command` are plain required strings/vectors; only
/// `timeout_secs` carries a default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDeploymentTarget {
    name: String,
    command: Vec<String>,
    verify: Vec<Vec<String>>,
    timeout_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn write_tmp(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hephaestus.toml");
        std::fs::write(&path, body).expect("write config");
        (dir, path)
    }

    #[test]
    fn defaults_validate_cleanly_for_development() {
        let cfg = Config::load(None, no_env).expect("defaults must be valid");
        assert_eq!(cfg.environment, Environment::Development);
        assert!(!cfg.sandbox.allow_network);
    }

    #[test]
    fn file_overrides_defaults() {
        let (_dir, path) = write_tmp(concat!(
            "[server]",
            "
",
            "port = 8000",
            "
"
        ));
        let cfg = Config::load(Some(&path), no_env).expect("file config must load");
        assert_eq!(cfg.server.port, 8000);
    }

    #[test]
    fn env_overrides_file() {
        let (_dir, path) = write_tmp(concat!(
            "[server]",
            "
",
            "port = 8000",
            "
"
        ));
        let cfg = Config::load(Some(&path), |k| {
            (k == "HEPHAESTUS_PORT").then(|| "9001".to_string())
        })
        .expect("must load");
        assert_eq!(cfg.server.port, 9001);
    }

    #[test]
    fn unknown_file_keys_rejected() {
        let (_dir, path) = write_tmp(concat!(
            "[server]",
            "
",
            "porrt = 1",
            "
"
        ));
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert_eq!(err.code(), "CONFIG_INVALID");
    }

    #[test]
    fn production_rejects_auth_disabled_and_local_db() {
        let cfg = Config {
            environment: Environment::Production,
            ..Default::default()
        };
        let Err(err) = cfg.validate() else {
            panic!("production validation must fail for default config");
        };
        assert!(err.to_string().contains("production"), "got: {err}");
    }

    #[test]
    fn non_postgres_scheme_rejected() {
        let mut cfg = Config::default();
        cfg.database.url = "mysql://localhost/x".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn redacted_url_strips_credentials() {
        let cfg = DatabaseConfig {
            url: "postgres://admin:hunter2@db.internal:5432/hephaestus".into(),
            max_connections: 5,
        };
        let red = cfg.redacted_url();
        assert!(!red.contains("hunter2"), "password leaked: {red}");
        assert!(red.contains("db.internal"));
    }

    #[test]
    fn storage_root_env_override() {
        let cfg = Config::load(None, |k| {
            (k == "HEPHAESTUS_STORAGE_ROOT").then(|| "/var/lib/hephaestus".to_string())
        })
        .expect("valid");
        assert_eq!(cfg.storage.root, PathBuf::from("/var/lib/hephaestus"));
    }

    const ORG_A: &str = "018f3c1e-0000-7000-8000-00000000000a";
    const ORG_B: &str = "018f3c1e-0000-7000-8000-00000000000b";

    fn key_toml(org: &str, token: &str) -> String {
        format!(
            "[[auth.keys]]\ntoken = \"{token}\"\norganization_id = \"{org}\"\nprincipal = \"ci-bot\"\n"
        )
    }

    #[test]
    fn api_keys_load_from_file_and_debug_redacts() {
        let body = format!(
            "[auth]\n{}",
            key_toml(ORG_A, "token-value-at-least-16-chars")
        );
        let (_dir, path) = write_tmp(&body);
        let cfg = Config::load(Some(&path), no_env).expect("valid config with keys");
        assert_eq!(cfg.auth.keys.len(), 1);
        let key = &cfg.auth.keys[0];
        assert_eq!(key.token, "token-value-at-least-16-chars");
        assert_eq!(key.principal, "ci-bot");
        assert_eq!(key.organization_id.to_string(), ORG_A);
        let debug = format!("{cfg:?}");
        assert!(
            !debug.contains("token-value-at-least-16-chars"),
            "token leaked in Debug: {debug}"
        );
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn api_keys_env_override_replaces_set() {
        let env_value =
            format!("bot-one,{ORG_A},env-token-one-16ch;bot-two,{ORG_B},env-token-two-16ch");
        let cfg = Config::load(None, |k| {
            (k == "HEPHAESTUS_AUTH_KEYS").then(|| env_value.clone())
        })
        .expect("valid config with env keys");
        assert_eq!(cfg.auth.keys.len(), 2);
        assert_eq!(cfg.auth.keys[0].principal, "bot-one");
        assert_eq!(cfg.auth.keys[1].organization_id.to_string(), ORG_B);
    }

    #[test]
    fn duplicate_tokens_rejected() {
        let body = format!(
            "[auth]\n{}{}",
            key_toml(ORG_A, "duplicate-token-16ch"),
            key_toml(ORG_B, "duplicate-token-16ch")
        );
        let (_dir, path) = write_tmp(&body);
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert_eq!(err.code(), "CONFIG_INVALID");
        assert!(err.to_string().contains("duplicate"), "got: {err}");
    }

    #[test]
    fn short_token_rejected() {
        let body = format!("[auth]\n{}", key_toml(ORG_A, "short"));
        let (_dir, path) = write_tmp(&body);
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert_eq!(err.code(), "CONFIG_INVALID");
        assert!(err.to_string().contains("16 characters"), "got: {err}");
    }

    #[test]
    fn empty_principal_rejected() {
        let mut cfg = Config::default();
        cfg.auth.keys.push(ApiKey {
            token: "long-enough-token-16".into(),
            organization_id: ORG_A.parse().expect("uuid"),
            principal: "   ".into(),
        });
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.code(), "CONFIG_INVALID");
    }

    #[test]
    fn production_requires_keys_when_auth_enabled() {
        let cfg = Config {
            environment: Environment::Production,
            auth: AuthConfig {
                disabled: false,
                ..Default::default()
            },
            database: DatabaseConfig {
                url: "postgres://db.internal:5432/hephaestus".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let Err(err) = cfg.validate() else {
            panic!("production without keys must fail validation");
        };
        assert!(err.to_string().contains("auth.keys"), "got: {err}");
    }

    #[test]
    fn production_accepts_configured_keys() {
        let cfg = Config {
            environment: Environment::Production,
            auth: AuthConfig {
                disabled: false,
                keys: vec![ApiKey {
                    token: "production-token-16".into(),
                    organization_id: ORG_A.parse().expect("uuid"),
                    principal: "operator".into(),
                }],
                ..Default::default()
            },
            database: DatabaseConfig {
                url: "postgres://db.internal:5432/hephaestus".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.validate().expect("valid production config");
    }

    #[test]
    fn worker_defaults_serve_deterministic_queues_only() {
        let cfg = Config::load(None, no_env).expect("defaults must be valid");
        assert_eq!(
            cfg.worker.queues,
            vec!["analysis", "verification", "build"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
        assert!(!cfg.worker.queues.contains(&"deployment".to_string()));
        // Unconfigured model is fine while only deterministic queues run.
        assert_eq!(cfg.model.base_url, "");
    }

    #[test]
    fn file_model_and_worker_sections_parse() {
        let (_dir, path) = write_tmp(
            "[model]\nbase_url = \"https://gateway.internal/v1\"\napi_key = \"secret\"\nmodel = \"forge-large\"\ntimeout_secs = 30\n\n[worker]\nid = \"worker-a\"\nqueues = [\"analysis\", \"planning\"]\nlease_ttl_secs = 60\npoll_interval_ms = 250\nconcurrency = 2\n",
        );
        let cfg = Config::load(Some(&path), no_env).expect("must load");
        assert_eq!(cfg.model.base_url, "https://gateway.internal/v1");
        assert_eq!(cfg.model.api_key.as_deref(), Some("secret"));
        assert_eq!(cfg.model.model, "forge-large");
        assert_eq!(cfg.model.timeout_secs, 30);
        assert_eq!(cfg.worker.id, "worker-a");
        assert_eq!(
            cfg.worker.queues,
            vec!["analysis".to_string(), "planning".to_string()]
        );
        assert_eq!(cfg.worker.lease_ttl_secs, 60);
        assert_eq!(cfg.worker.poll_interval_ms, 250);
        assert_eq!(cfg.worker.concurrency, 2);
    }

    #[test]
    fn debug_output_redacts_model_api_key() {
        let mut cfg = Config::default();
        cfg.model.api_key = Some("super-secret-value".into());
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("super-secret-value"), "got: {rendered}");
        assert!(rendered.contains("[redacted]"));
    }

    #[test]
    fn env_overrides_model_and_worker() {
        let cfg = Config::load(None, |k| match k {
            "HEPHAESTUS_MODEL_BASE_URL" => Some("http://127.0.0.1:9/v1".into()),
            "HEPHAESTUS_MODEL_API_KEY" => Some("env-key".into()),
            "HEPHAESTUS_MODEL_NAME" => Some("m".into()),
            "HEPHAESTUS_WORKER_QUEUES" => Some("analysis, verification ,".into()),
            _ => None,
        })
        .expect("must load");
        assert_eq!(cfg.model.base_url, "http://127.0.0.1:9/v1");
        assert_eq!(cfg.model.api_key.as_deref(), Some("env-key"));
        assert_eq!(cfg.model.model, "m");
        assert_eq!(
            cfg.worker.queues,
            vec!["analysis".to_string(), "verification".to_string()]
        );
    }

    #[test]
    fn unknown_queue_rejected() {
        let (_dir, path) = write_tmp("[worker]\nqueues = [\"anlysis\"]\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert_eq!(err.code(), "CONFIG_INVALID");
        assert!(err.to_string().contains("unknown queue"), "got: {err}");
    }

    /// The deployment queue stays unservable until a target exists -
    /// capacity for a queue that can never receive jobs would be
    /// simulation by another name (ADR-013).
    #[test]
    fn deployment_queue_rejected_without_targets() {
        let (_dir, path) = write_tmp("[worker]\nqueues = [\"analysis\", \"deployment\"]\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("deployment"), "got: {err}");
    }

    const STAGING_TARGET: &str = "[[deployment.targets]]
         name = \"staging\"
         command = [\"heph-deploy\", \"--env\", \"staging\"]
         verify = [[\"heph-probe\", \"--ready\"]]
";

    #[test]
    fn deployment_queue_loads_with_configured_target() {
        let (_dir, path) = write_tmp(&format!(
            "[worker]\nqueues = [\"analysis\", \"deployment\"]\n\n{STAGING_TARGET}"
        ));
        let cfg = Config::load(Some(&path), no_env).expect("targets satisfy the precondition");
        assert_eq!(cfg.deployment.targets.len(), 1);
        let target = cfg.deployment.find("staging").expect("target by name");
        assert_eq!(
            target.command,
            vec!["heph-deploy".to_string(), "--env".into(), "staging".into()]
        );
        assert_eq!(target.timeout_secs, DEFAULT_DEPLOY_TIMEOUT_SECS);
    }

    #[test]
    fn duplicate_target_names_rejected() {
        let (_dir, path) = write_tmp(&format!("{STAGING_TARGET}{STAGING_TARGET}"));
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("duplicate name"), "got: {err}");
    }

    #[test]
    fn target_without_verify_hooks_rejected() {
        let body =
            "[[deployment.targets]]\nname = \"prod\"\ncommand = [\"deploy.sh\"]\nverify = []\n";
        let (_dir, path) = write_tmp(body);
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("simulation risk"), "got: {err}");
    }

    #[test]
    fn target_path_like_program_rejected() {
        let body = "[[deployment.targets]]\nname = \"prod\"\n\
             command = [\"/bin/sh\", \"-c\", \"ship it\"]\n\
             verify = [[\"probe-ok\"]]\n";
        let (_dir, path) = write_tmp(body);
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(
            err.to_string().contains("bare executable name"),
            "got: {err}"
        );
    }

    #[test]
    fn target_timeout_out_of_bounds_rejected() {
        let body = "[[deployment.targets]]\nname = \"prod\"\ncommand = [\"heph-deploy\"]\n\
             verify = [[\"heph-probe\"]]\ntimeout_secs = 2\n";
        let (_dir, path) = write_tmp(body);
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("timeout_secs"), "got: {err}");
    }

    #[test]
    fn duplicate_queues_rejected() {
        let (_dir, path) = write_tmp("[worker]\nqueues = [\"build\", \"build\"]\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "got: {err}");
    }

    #[test]
    fn empty_queue_set_rejected() {
        let (_dir, path) = write_tmp("[worker]\nqueues = []\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "got: {err}");
    }

    #[test]
    fn model_backed_queue_requires_provider() {
        for queue in ["planning", "implementation", "review"] {
            let body = format!("[worker]\nqueues = [\"{queue}\"]\n");
            let (_dir, path) = write_tmp(&body);
            let err = Config::load(Some(&path), no_env).unwrap_err();
            assert!(
                err.to_string().contains("model.base_url"),
                "{queue}: got {err}"
            );

            let body = format!(
                "[model]\nbase_url = \"http://127.0.0.1:9/v1\"\n[worker]\nqueues = [\"{queue}\"]\n"
            );
            let (_dir, path) = write_tmp(&body);
            let err = Config::load(Some(&path), no_env).unwrap_err();
            assert!(
                err.to_string().contains("model.model"),
                "{queue}: got {err}"
            );
        }
    }

    #[test]
    fn deterministic_only_worker_needs_no_model() {
        let (_dir, path) =
            write_tmp("[worker]\nqueues = [\"analysis\", \"verification\", \"build\"]\n");
        let cfg = Config::load(Some(&path), no_env).expect("no model needed");
        assert!(cfg.model.base_url.is_empty());
    }

    #[test]
    fn lease_ttl_bounds_rejected() {
        let (_dir, path) = write_tmp("[worker]\nlease_ttl_secs = 5\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("lease_ttl_secs"), "got: {err}");

        let (_dir, path) = write_tmp("[worker]\nlease_ttl_secs = 3601\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("lease_ttl_secs"), "got: {err}");
    }

    #[test]
    fn concurrency_bounds_rejected() {
        let (_dir, path) = write_tmp("[worker]\nconcurrency = 0\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(err.to_string().contains("concurrency"), "got: {err}");
    }

    #[test]
    fn production_demands_model_key_for_model_queues() {
        let cfg = Config {
            environment: Environment::Production,
            auth: AuthConfig {
                keys: vec![ApiKey {
                    token: "production-token-16".into(),
                    organization_id: ORG_A.parse().expect("uuid"),
                    principal: "operator".into(),
                }],
                ..Default::default()
            },
            database: DatabaseConfig {
                url: "postgres://db.internal:5432/hephaestus".into(),
                ..Default::default()
            },
            model: ModelConfig {
                base_url: "https://gateway.internal/v1".into(),
                model: "forge-large".into(),
                api_key: None,
                ..Default::default()
            },
            worker: WorkerSettings {
                queues: vec!["planning".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let Err(err) = cfg.validate() else {
            panic!("production without model key must fail validation");
        };
        assert!(err.to_string().contains("model.api_key"), "got: {err}");
    }

    #[test]
    fn web_section_parses_from_file() {
        let (_dir, path) = write_tmp(
            "[web]\ndist_dir = \"site\"\n[web.runtime_config]\nbase_url = \"\"\ntoken = \"dash-token-1\"\npoll_seconds = 5\n",
        );
        let cfg = Config::load(Some(&path), no_env).expect("valid web config");
        assert_eq!(
            cfg.web.dist_dir.as_deref(),
            Some(std::path::Path::new("site"))
        );
        let rc = cfg.web.runtime_config.as_ref().expect("runtime config");
        assert_eq!(rc.token, "dash-token-1");
        assert_eq!(rc.poll_seconds, 5);
        assert_eq!(rc.base_url, "");
    }

    #[test]
    fn request_timeout_bounds_rejected() {
        let (_dir, path) = write_tmp("[server]\nrequest_timeout_secs = 0\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(
            err.to_string().contains("request_timeout_secs"),
            "got: {err}"
        );

        let (_dir, path) = write_tmp("[server]\nrequest_timeout_secs = 601\n");
        let err = Config::load(Some(&path), no_env).unwrap_err();
        assert!(
            err.to_string().contains("request_timeout_secs"),
            "got: {err}"
        );

        let (_dir, path) = write_tmp("[server]\nrequest_timeout_secs = 600\n");
        Config::load(Some(&path), no_env).expect("600 is the inclusive bound");
    }

    #[test]
    fn web_token_env_creates_runtime_config() {
        let env = |name: &str| match name {
            "HEPHAESTUS_WEB_TOKEN" => Some("env-token-16".to_string()),
            _ => None,
        };
        let cfg = Config::load(None, env).expect("valid");
        let rc = cfg
            .web
            .runtime_config
            .as_ref()
            .expect("runtime config from env");
        assert_eq!(rc.token, "env-token-16");
        // Unspecified companions keep their fail-closed defaults.
        assert_eq!(rc.base_url, "");
        assert_eq!(rc.poll_seconds, 10);
        assert!(cfg.web.dist_dir.is_none());
    }

    #[test]
    fn web_runtime_debug_redacts_token() {
        let rc = WebRuntimeConfig {
            base_url: String::new(),
            token: "super-secret-token".into(),
            poll_seconds: 10,
        };
        let rendered = format!("{rc:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "leaked: {rendered}"
        );
        assert!(rendered.contains("[redacted]"), "got: {rendered}");
    }

    #[test]
    fn default_web_config_serves_nothing() {
        let cfg = Config::default();
        assert!(cfg.web.dist_dir.is_none());
        assert!(cfg.web.runtime_config.is_none());
        assert_eq!(cfg.server.request_timeout_secs, 30);
    }
}
