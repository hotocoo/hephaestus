//! # hephaestus-db
//!
//! Persistence layer for Hephaestus: PostgreSQL connection management,
//! versioned migrations, and typed stores enforcing tenant scoping on
//! every query.
//!
//! Rules:
//! * Schema changes only through numbered migration files.
//! * Every organization-scoped query filters by organization_id.
//! * Writes that must not duplicate work take idempotency keys and are
//!   enforced by unique indexes, not by check-then-insert races.

pub mod audit;
pub mod catalog;
pub mod delivery;
pub mod deployment;
pub mod events;
pub mod execution;
pub mod intake;
pub mod jobs;
pub mod planning;
pub mod store;
pub mod tasks;
pub mod workflow;

pub use store::Db;

/// Map a sqlx error onto the core taxonomy without leaking internals.
pub(crate) fn map_sqlx(e: sqlx::Error) -> hephaestus_core::Error {
    match &e {
        sqlx::Error::RowNotFound => hephaestus_core::Error::NotFound { entity: "row" },
        sqlx::Error::Database(db) if db.constraint().is_some() => {
            let constraint = db.constraint().unwrap_or_default();
            if constraint.contains("idem") || constraint.contains("slug_uidx") {
                hephaestus_core::Error::Conflict {
                    message: "a resource with this identity already exists".into(),
                }
            } else {
                hephaestus_core::Error::Conflict {
                    message: "operation conflicts with current state".into(),
                }
            }
        }
        _ => {
            // Log details server-side; return opaque storage error.
            tracing::error!(error = %e, "database error");
            hephaestus_core::Error::Storage(Box::new(e))
        }
    }
}

#[cfg(test)]
mod testutil;
