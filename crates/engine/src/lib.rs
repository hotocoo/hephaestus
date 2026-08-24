//! # hephaestus-engine
//!
//! Workflow orchestration and durable worker execution.
//!
//! The engine turns task intake into workflow runs and drives them by
//! claiming durable jobs from the queue. Handlers implement
//! [`JobHandler`] per payload kind; the worker owns leases,
//! heartbeats, retries and graceful shutdown so handlers stay simple.

pub mod intake;
pub mod jobs;
pub mod worker;

pub use intake::IntakeService;
pub use jobs::{DecodeError, JobPayload, Queue};
pub use worker::{Worker, WorkerConfig};
