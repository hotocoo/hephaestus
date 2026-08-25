//! # hephaestus-engine
//!
//! Workflow orchestration and durable worker execution.
//!
//! The engine turns task intake into workflow runs and drives them by
//! claiming durable jobs from the queue. Handlers implement
//! [`JobHandler`] per payload kind; the worker owns leases,
//! heartbeats, retries and graceful shutdown so handlers stay simple.

pub mod analysis;
pub mod approval;
pub mod delivery;
pub mod execution;
pub mod governed;
pub mod intake;
pub mod jobs;
pub mod planning;
pub mod review;
pub mod verification;
pub mod worker;

pub use analysis::{AnalysisHandler, StageError, WorkspaceLayout};
pub use approval::{ApprovalDecisionInput, ApprovalOutcome, ApprovalService};
pub use delivery::{
    ARTIFACT_DIR, BUILD_COMMAND, BuildHandler, MergeDecisionInput, MergeOutcome, MergeService,
};
pub use execution::{
    ExecutionHandler, ImplementationDocument, MAX_FIX_ROUNDS, RepairHandler, StepOutcomeDocument,
};
pub use governed::SessionDeps;
pub use intake::IntakeService;
pub use jobs::{DecodeError, JobPayload, Queue, RepairCause};
pub use planning::{ExtractionHandler, PLAN_PROMPT_VERSION, PlanningHandler};
pub use review::{MAX_REVIEW_ROUNDS, REVIEW_DIFF_MAX_CHARS, ReviewHandler, ReviewVerdictDocument};
pub use verification::{
    RunVerificationHandler, SUPPORTED_LAYERS, VerificationLayerError, default_layer_plan,
    layer_spec_for,
};
pub use worker::{HandlerOutcome, HandlerRegistry, Worker, WorkerConfig};
