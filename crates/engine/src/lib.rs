//! # hephaestus-engine
//!
//! Workflow orchestration and durable worker execution.
//!
//! The engine turns task intake into workflow runs and drives them by
//! claiming durable jobs from the queue. Handlers implement
//! [`JobHandler`] per payload kind; the worker owns leases,
//! heartbeats, retries and graceful shutdown so handlers stay simple.

pub mod analysis;
pub mod intake;
pub mod jobs;
pub mod planning;
pub mod worker;

pub use analysis::{AnalysisHandler, StageError, WorkspaceLayout};
pub use intake::IntakeService;
pub use jobs::{DecodeError, JobPayload, Queue};
pub use planning::{ExtractionHandler, PLAN_PROMPT_VERSION, PlannerDeps, PlanningHandler};
pub use worker::{HandlerRegistry, Worker, WorkerConfig};
