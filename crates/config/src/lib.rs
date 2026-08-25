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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1".into(),
            port: 7300,
            workers: 0,
            max_body_bytes: 8 * 1024 * 1024,
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
        if let Some(v) = env("HEPHAESTUS_AUTH_KEYS") {
            self.auth.keys = parse_keys_env(&v);
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
        }
        Ok(())
    }
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
    environment: Option<Environment>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileServer {
    bind_addr: Option<String>,
    port: Option<u16>,
    workers: Option<usize>,
    max_body_bytes: Option<usize>,
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
}
