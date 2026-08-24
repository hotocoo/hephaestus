//! Error taxonomy for Hephaestus.
//!
//! Errors are structured so that API layers can map them to stable
//! public codes without exposing internals (stack traces, connection
//! strings, secrets).

use thiserror::Error;

/// Convenience alias used across crates.
pub type Result<T> = std::result::Result<T, Error>;

/// Core error type. Library crates convert their specific errors into
/// this taxonomy; binaries map it onto transport-level responses.
#[derive(Debug, Error)]
pub enum Error {
    /// Input failed validation.
    #[error("validation failed: {field}: {message}")]
    Validation {
        /// Name of the offending field or concept.
        field: String,
        /// Human-readable explanation (safe to expose).
        message: String,
    },

    /// The requested entity does not exist (or the caller may not see it).
    #[error("{entity} not found")]
    NotFound {
        /// Entity kind, e.g. "task".
        entity: &'static str,
    },

    /// The operation conflicts with current persistent state.
    #[error("conflict: {message}")]
    Conflict {
        /// Explanation safe to expose to callers.
        message: String,
    },

    /// An authorization check denied the action.
    #[error("forbidden: {reason}")]
    Forbidden {
        /// Why access was denied (safe wording; no internals).
        reason: String,
    },

    /// Authentication is required or the presented credentials are invalid.
    #[error("authentication required")]
    Unauthenticated,

    /// Illegal workflow transition was rejected.
    #[error("illegal transition {from} -{event:?}-> : {reason}")]
    IllegalTransition {
        /// Current state name.
        from: String,
        /// Attempted trigger event.
        event: crate::state::TransitionEvent,
        /// Rejection reason (safe to expose).
        reason: String,
    },

    /// A deterministic verification layer reported failure.
    #[error("verification failed: {layer}")]
    VerificationFailed {
        /// Which verification layer failed.
        layer: &'static str,
    },

    /// Configuration was rejected during validation.
    #[error("configuration error: {0}")]
    Config(String),

    /// Persistence layer failure. Details are logged, never returned
    /// verbatim to clients.
    #[error("storage error")]
    Storage(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// External system call failed.
    #[error("external error: {system}")]
    External {
        /// Which external system (e.g. "model-provider", "github").
        system: &'static str,
        /// Underlying cause; logged internally, never returned verbatim.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Budgets or limits were exceeded and the operation stopped safely.
    #[error("budget exhausted: {what}")]
    BudgetExhausted {
        /// What limit was hit (time/tokens/tool-calls/...).
        what: &'static str,
    },
}

impl Error {
    /// Stable machine-readable error code used in API responses.
    ///
    /// These codes are part of the public API contract and must not
    /// be renamed between minor versions.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Validation { .. } => "VALIDATION_FAILED",
            Error::NotFound { .. } => "NOT_FOUND",
            Error::Conflict { .. } => "CONFLICT",
            Error::Forbidden { .. } => "FORBIDDEN",
            Error::Unauthenticated => "UNAUTHENTICATED",
            Error::IllegalTransition { .. } => "WORKFLOW_ILLEGAL_TRANSITION",
            Error::VerificationFailed { .. } => "VERIFICATION_FAILED",
            Error::Config(_) => "CONFIG_INVALID",
            Error::Storage(_) => "STORAGE_ERROR",
            Error::External { .. } => "EXTERNAL_ERROR",
            Error::BudgetExhausted { .. } => "BUDGET_EXHAUSTED",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn codes_are_stable_and_snake_upper() {
        let e = Error::NotFound { entity: "task" };
        assert_eq!(e.code(), "NOT_FOUND");
        assert_eq!(e.to_string(), "task not found");
        let v = Error::Validation {
            field: "title".into(),
            message: "must not be empty".into(),
        };
        assert_eq!(v.code(), "VALIDATION_FAILED");
    }

    #[test]
    fn storage_error_does_not_leak_source_in_display() {
        let inner = std::io::Error::other("postgres://user:hunter2@db.internal/prod");
        let e = Error::Storage(Box::new(inner));
        let rendered = e.to_string();
        assert!(
            !rendered.contains("hunter2"),
            "secrets must not leak via Display"
        );
        assert_eq!(rendered, "storage error");
    }
}
