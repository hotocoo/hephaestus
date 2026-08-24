//! # hephaestus-core
//!
//! Foundational domain model for Hephaestus: identifiers, timestamps,
//! error taxonomy, the workflow state machine, and the versioned
//! internal event model.
//!
//! This crate has minimal dependencies so it can be used by every
//! other crate, including security-sensitive ones.
//!
//! Design rules enforced here:
//! * All entity IDs are strongly typed newtypes over UUIDv7
//!   (time-ordered, globally unique, safe to sort by creation).
//! * The workflow state machine is a total function from
//!   `(state, event) -> Option<state>`; illegal transitions are
//!   unrepresentable rather than runtime errors.
//! * Every event carries a schema version and a provenance tag.

pub mod domain;
pub mod error;
pub mod event;
pub mod id;
pub mod injection;
pub mod state;

pub use error::{Error, Result};
pub use id::HephaestusId;

/// Semantic version of the Hephaestus core domain model.
///
/// Bumped when event or state-machine schemas change
/// incompatibly. See `docs/adr` for the compatibility policy.
pub const DOMAIN_SCHEMA_VERSION: u32 = 1;
