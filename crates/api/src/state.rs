//! Shared handler state.

use std::sync::Arc;

use hephaestus_db::Db;
use hephaestus_engine::approval::ApprovalService;
use hephaestus_engine::delivery::MergeService;
use hephaestus_engine::intake::IntakeService;

use crate::auth::AuthPolicy;

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
            db,
        }
    }
}
