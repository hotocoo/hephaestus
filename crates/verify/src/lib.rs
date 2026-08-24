//! # hephaestus-verify
//!
//! Layered deterministic verification (spec: no successful state may be
//! declared unless every required layer passes).
//!
//! A [`VerificationPlan`] is an ordered list of layers; each layer is
//! a command executed through the governed tool runtime - capabilities,
//! allowlists, timeouts and audit all apply. There is deliberately no
//! API to run "verification" outside this path.

pub mod plan;
pub mod report;

pub use plan::{Layer, LayerSpec, VerificationPlan};
pub use report::{LayerResult, VerificationReport, run_plan};
