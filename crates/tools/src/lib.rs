//! # hephaestus-tools
//!
//! Centralized tool runtime (ADR-005, ADR-006).
//!
//! Every tool invocation passes through:
//! identity -> capability check -> policy hook -> execution -> audit.
//!
//! There is no side door: agents receive capability SETS, the runtime
//! enforces them outside the model, and each decision is recorded in
//! the tamper-evident audit log.

pub mod capabilities;
pub mod registry;
pub mod shell;

pub use capabilities::{CapabilitySet, ToolCapability};
pub use registry::{ToolDefinition, ToolRegistry};
pub use shell::SandboxedShell;
