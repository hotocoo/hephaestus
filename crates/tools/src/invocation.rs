//! Audited invocation entry point.
//!
//! The ONLY way agents invoke tools. Each call records who, what,
//! target, and the authorization decision into the tamper-evident
//! audit log - before the tool body runs on grants, after refusals.

use hephaestus_core::Result;
use hephaestus_db::Db;
use hephaestus_db::audit::AuditEntry;

use crate::capabilities::{CapabilitySet, ToolCapability};
use crate::registry::ToolRegistry;

/// Execute one governed filesystem or shell action.
///
/// `detail` must already be secret-free; this layer adds no redaction
/// (defense-in-depth redaction exists upstream in core).
pub struct Invocation<'a> {
    /// Acting principal (agent run id, "system", user id...).
    pub actor: &'a str,
    /// Tool name as registered ("fs.read", "shell.exec", ...).
    pub tool: &'a str,
}

/// Outcome of the authorization step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzDecision {
    /// Capability present: proceed.
    Granted,
    /// Capability absent: refuse before execution.
    Denied,
}

impl Invocation<'_> {
    /// Check authorization for a capability against the registry.
    ///
    /// Unknown tools deny by default even if a caller passes a valid
    /// capability - registration is part of governance.
    pub fn authorize(
        &self,
        registry: &ToolRegistry,
        caps: &CapabilitySet,
        needed: Option<ToolCapability>,
    ) -> AuthzDecision {
        match registry.required_capability(self.tool) {
            Some(required) => {
                let ok = caps.has(needed.unwrap_or(required));
                if ok {
                    AuthzDecision::Granted
                } else {
                    AuthzDecision::Denied
                }
            }
            None => AuthzDecision::Denied,
        }
    }

    /// Record an invocation decision in the audit chain.
    pub async fn audit(
        &self,
        db: &Db,
        decision: AuthzDecision,
        detail: serde_json::Value,
    ) -> Result<i64> {
        let (verb, action) = match decision {
            AuthzDecision::Granted => ("granted", self.tool),
            AuthzDecision::Denied => ("denied", self.tool),
        };
        db.audit(&AuditEntry {
            actor: self.actor,
            action: "tool.invoke",
            target_type: "tool",
            target_id: Some(action),
            detail: serde_json::json!({
                "decision": verb,
                "tool": self.tool,
                "extra": detail,
            }),
        })
        .await
    }
}
