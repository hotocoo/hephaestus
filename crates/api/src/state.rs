//! Shared handler state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hephaestus_db::Db;
use hephaestus_engine::approval::ApprovalService;
use hephaestus_engine::delivery::MergeService;
use hephaestus_engine::intake::IntakeService;

use crate::auth::AuthPolicy;
use crate::web_static::WebSite;

/// Wall-clock default for one request when configuration stays silent.
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// State shared by every route.
#[derive(Clone)]
pub struct AppState {
    /// Durable storage handle.
    pub db: Db,
    /// Task intake service (validation + atomic bootstrap).
    pub intake: IntakeService,
    /// Plan approval gate decisions.
    pub approvals: ApprovalService,
    /// Merge gate decisions.
    pub merges: MergeService,
    /// Authentication policy.
    pub auth: Arc<AuthPolicy>,
    /// Request body size limit from configuration.
    pub max_body_bytes: usize,
    /// Request wall-clock cap from configuration (ADR-014).
    pub request_timeout_secs: u64,
    /// Loaded dashboard site; None serves API-only responses.
    pub web_site: Option<Arc<WebSite>>,
    /// Storage root for artifact serving (ADR-015); None refuses the
    /// artifact endpoints loudly instead of pretending to read.
    pub storage_root: Option<PathBuf>,
}

impl AppState {
    /// Assemble state from a database handle and auth policy.
    ///
    /// `max_body_bytes` comes from server configuration and bounds
    /// every request body. The decision-only constructors keep
    /// model-provider credentials out of the API process; sessions run
    /// in workers.
    pub fn new(db: Db, auth: AuthPolicy, max_body_bytes: usize) -> Self {
        Self {
            intake: IntakeService::new(db.clone()),
            approvals: ApprovalService::gate_only(),
            merges: MergeService,
            auth: Arc::new(auth),
            max_body_bytes,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            web_site: None,
            storage_root: None,
            db,
        }
    }

    /// Set the request wall-clock cap (seconds) from configuration.
    pub fn with_request_timeout(mut self, secs: u64) -> Self {
        self.request_timeout_secs = secs;
        self
    }

    /// Attach a loaded dashboard for static serving (ADR-014).
    pub fn with_web_site(mut self, site: WebSite) -> Self {
        self.web_site = Some(Arc::new(site));
        self
    }

    /// Configure the storage root artifacts are served from (ADR-015).
    pub fn with_storage_root(mut self, root: PathBuf) -> Self {
        self.storage_root = Some(root);
        self
    }

    /// The configured request timeout as a duration.
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }
}
