//! # hephaestus-agent
//!
//! Agent runtime (ADR-005): who agents are, what they may do, and the
//! only loop through which they act.
//!
//! Three pieces, each fail-closed:
//!
//! * role - declarative role manifests. Capabilities are data;
//!   validation refuses manifests that exceed their role's policy.
//! * provider - the model-provider boundary. Providers answer
//!   prompts; they can never execute anything themselves.
//! * session - the governed turn loop. Every proposed tool call is
//!   authorized by the tool runtime, audited through a DecisionSink,
//!   and executed with capability-bound tools. Model output is data:
//!   it selects among pre-approved tools or ends the run; it never
//!   widens its own permissions.
//!
//! There is no path from a model response to capability state.

pub mod provider;
pub mod role;
pub mod session;

pub use provider::{
    ChatMessage, CompletionRequest, CompletionResponse, ModelProvider, OpenAiCompatProvider,
    PromptRole, ProviderConfig,
};
pub use role::{AgentRole, RoleManifest};
pub use session::{
    AgentSession, AuditLogSink, CollectingSink, DecisionRecord, DecisionSink, RunSpec,
    SessionLimits, SessionOutcome, SessionStatus, UntrustedFact,
};
