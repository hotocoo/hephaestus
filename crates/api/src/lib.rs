//! # hephaestus-api
//!
//! HTTP API layer (ADR-009): the only component the web tier talks to.
//!
//! The crate owns routing, request/response shapes, authentication and
//! error mapping. Handlers contain no business rules; they call the
//! existing engine services and org-scoped store reads, so every
//! durable effect keeps its service-level idempotency and audit trail.
//!
//! Design rules:
//! * The authenticated principal carries its organization; handlers
//!   never accept tenant scope from request bodies.
//! * Errors map onto the core taxonomy with stable public codes;
//!   internals are logged, never serialized.
//! * Gate decisions go through ApprovalService / MergeService, never
//!   around them.

pub mod auth;
pub mod dto;
pub mod error;
pub mod openapi;
pub mod routes;
pub mod state;
pub mod web_static;

pub use auth::{AuthPolicy, Principal};
pub use error::{ApiError, ApiResult};
pub use routes::router;
pub use state::AppState;
pub use web_static::WebSite;
