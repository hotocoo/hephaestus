//! Shared plumbing for governed agent sessions inside pipeline
//! stages.
//!
//! Every model-backed stage (planning, implementation, repair,
//! review) runs through the same shape: a role manifest binds a
//! capability set, the session loop authorizes and audits every tool
//! call, and the stage's final answer must be EXACTLY one JSON
//! document parsed deterministically below. Nothing the model says
//! widens permissions; malformed output fails into bounded queue
//! retries rather than being guessed at.

use std::sync::Arc;

use serde::de::DeserializeOwned;

use hephaestus_agent::provider::ModelProvider;
use hephaestus_agent::role::{AgentRole, RoleManifest};
use hephaestus_agent::session::{
    AgentSession, DecisionSink, RunSpec, SessionLimits, SessionStatus, UntrustedFact,
};
use hephaestus_core::{Error, Result};
use hephaestus_tools::registry::ToolRegistry;

use crate::analysis::StageError;

/// Everything a governed stage needs beyond its job payload: model
/// boundary, audit destination, tool registry and budgets.
#[derive(Clone)]
pub struct SessionDeps {
    /// Model boundary.
    pub provider: Arc<dyn ModelProvider>,
    /// Where authorization decisions are durably recorded.
    pub sink: Arc<dyn DecisionSink>,
    /// Registered tools (built-ins only today).
    pub registry: Arc<ToolRegistry>,
    /// Model identifier served by the provider.
    pub model: String,
    /// Run budgets.
    pub limits: SessionLimits,
}

impl SessionDeps {
    /// Dependencies with built-in registry and default budgets.
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        sink: Arc<dyn DecisionSink>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            sink,
            registry: Arc::new(ToolRegistry::with_builtins()),
            model: model.into(),
            limits: SessionLimits::default(),
        }
    }

    /// The actor label used in audit entries for one role.
    pub fn actor_for(role: AgentRole) -> String {
        format!("agent:{}", role.as_str())
    }
}

/// Run one governed session for one role over one workspace and
/// return its final answer text.
///
/// Manifest validation failures are configuration (permanent);
/// provider or budget problems are transient (retryable within queue
/// budgets); a session stopped by policy is permanent - retrying
/// cannot make denials go away.
pub(crate) async fn run_role_session(
    deps: &SessionDeps,
    role: AgentRole,
    workspace: std::path::PathBuf,
    objective: &str,
    facts: &[UntrustedFact],
) -> std::result::Result<String, StageError> {
    let manifest = RoleManifest::built_in(role);
    let spec = RunSpec {
        model: deps.model.clone(),
        actor: SessionDeps::actor_for(role),
        provider: Arc::clone(&deps.provider),
        sink: Arc::clone(&deps.sink),
        limits: deps.limits.clone(),
    };
    // Manifest validation failure is configuration, not transience.
    let session = AgentSession::new(&manifest, &deps.registry, workspace, spec)
        .map_err(StageError::Permanent)?;
    let outcome = session
        .run(objective, facts)
        .await
        .map_err(StageError::Retryable)?;
    match outcome.status {
        SessionStatus::Completed => outcome.final_text.ok_or_else(|| {
            StageError::Permanent(Error::Validation {
                field: "session".into(),
                message: "completed without final text".into(),
            })
        }),
        SessionStatus::Failed { reason } => Err(StageError::Permanent(Error::Validation {
            field: "session".into(),
            message: format!("agent run stopped: {reason}"),
        })),
        SessionStatus::BudgetExhausted { what } => Err(StageError::Retryable(Error::Validation {
            field: "budget".into(),
            message: format!("agent run exhausted its {what} budget"),
        })),
    }
}

/// Parse a stage's final answer: exactly one JSON object, nothing
/// else. Trailing prose, code fences, or multiple objects fail loudly.
pub(crate) fn parse_strict<T: DeserializeOwned>(final_text: &str) -> Result<T> {
    serde_json::from_str(final_text.trim()).map_err(|e| Error::Validation {
        field: "final".into(),
        message: format!("stage final answer is not a valid document: {e}"),
    })
}
