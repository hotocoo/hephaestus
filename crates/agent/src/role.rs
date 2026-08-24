//! Agent roles and capability manifests (ADR-005).
//!
//! A role manifest is DATA describing what an agent run may do: which
//! registered tools it may name, which capabilities those tools need,
//! and - for shell access - which command names are allowlisted.
//!
//! Validation enforces policy invariants no prompt can rewrite:
//! reviewers cannot write or execute, verifiers cannot mutate sources,
//! no built-in role touches secrets, deployments or network egress,
//! and every granted capability must be required by some allowed tool
//! (least privilege, no dead grants).

use serde::{Deserialize, Serialize};

use hephaestus_core::{Error, Result};
use hephaestus_tools::capabilities::{CapabilitySet, ToolCapability};
use hephaestus_tools::registry::ToolRegistry;

/// The agent roles defined by policy.
///
/// Adding a role means adding a variant here AND extending
/// [RoleManifest::validate] with its separation rules - deliberately
/// a reviewed-code change, not configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Extracts requirements and drafts plans. Read-only.
    Planner,
    /// Implements approved steps inside the workspace.
    Implementer,
    /// Reviews changes. Read-only by hard invariant.
    Reviewer,
    /// Runs deterministic verification layers. Cannot mutate sources.
    Verifier,
}

impl AgentRole {
    /// Stable string used in audit entries and API payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Planner => "planner",
            AgentRole::Implementer => "implementer",
            AgentRole::Reviewer => "reviewer",
            AgentRole::Verifier => "verifier",
        }
    }
}

/// Declarative capability manifest for one agent role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleManifest {
    /// Which role this manifest belongs to.
    pub role: AgentRole,
    /// Human-readable purpose shown in audit views and docs.
    pub description: String,
    capabilities: Vec<ToolCapability>,
    allowed_tools: Vec<String>,
    allowed_commands: Vec<String>,
}

impl RoleManifest {
    /// The least-privilege built-in manifest for a role.
    ///
    /// Command allowlists here are defaults for a Rust-oriented
    /// checkout; operators narrow them per task, they never widen.
    pub fn built_in(role: AgentRole) -> Self {
        let (description, capabilities, allowed_tools, allowed_commands) = match role {
            AgentRole::Planner => (
                "Reads repository facts and drafts implementation plans.",
                vec![ToolCapability::WorkspaceRead],
                vec!["fs.read".to_string()],
                Vec::new(),
            ),
            AgentRole::Implementer => (
                "Implements approved plan steps inside the workspace.",
                vec![
                    ToolCapability::WorkspaceRead,
                    ToolCapability::WorkspaceWrite,
                    ToolCapability::ShellExec,
                ],
                vec![
                    "fs.read".to_string(),
                    "fs.write".to_string(),
                    "shell.exec".to_string(),
                ],
                vec!["cargo".to_string(), "rustfmt".to_string()],
            ),
            AgentRole::Reviewer => (
                "Reviews changes read-only; separation of duties lives in policy, not prompts.",
                vec![ToolCapability::WorkspaceRead],
                vec!["fs.read".to_string()],
                Vec::new(),
            ),
            AgentRole::Verifier => (
                "Runs deterministic verification layers against current sources.",
                vec![ToolCapability::WorkspaceRead, ToolCapability::ShellExec],
                vec!["fs.read".to_string(), "shell.exec".to_string()],
                vec!["cargo".to_string()],
            ),
        };
        Self {
            role,
            description: description.to_string(),
            capabilities,
            allowed_tools,
            allowed_commands,
        }
    }

    /// Registered tools this manifest may invoke.
    pub fn allowed_tools(&self) -> &[String] {
        &self.allowed_tools
    }

    /// Allowlisted bare command names for shell execution.
    pub fn allowed_commands(&self) -> &[String] {
        &self.allowed_commands
    }

    /// Validate this manifest against registry state and role policy.
    ///
    /// Every invariant fails closed with a specific message so
    /// misconfiguration is loud at startup, not at incident time.
    pub fn validate(&self, registry: &ToolRegistry) -> Result<()> {
        // 1) An agent that can name no tool is a configuration error.
        if self.allowed_tools.is_empty() {
            return Err(Error::Validation {
                field: "allowed_tools".into(),
                message: "manifest allows no tools".into(),
            });
        }

        // 2) Tools must exist in the registry: registration is part of
        //    governance; unknown names deny even with valid caps.
        for tool in &self.allowed_tools {
            if registry.get(tool).is_none() {
                return Err(Error::Validation {
                    field: "allowed_tools".into(),
                    message: format!("tool {tool:?} is not registered"),
                });
            }
        }

        // 3) No current role may hold administrative capabilities.
        //    Deploy/network/secret access belongs to future operator
        //    roles whose variants extend these checks explicitly.
        const FORBIDDEN_EVERYWHERE: [ToolCapability; 3] = [
            ToolCapability::SecretRead,
            ToolCapability::NetworkEgress,
            ToolCapability::Deploy,
        ];
        for cap in FORBIDDEN_EVERYWHERE {
            if self.capabilities.contains(&cap) {
                return Err(Error::Validation {
                    field: "capabilities".into(),
                    message: format!(
                        "role {} may not hold capability {}",
                        self.role.as_str(),
                        cap.as_str()
                    ),
                });
            }
        }

        // 4) Role separation, encoded in policy (ADR-005).
        let write = self.capabilities.contains(&ToolCapability::WorkspaceWrite);
        let shell = self.capabilities.contains(&ToolCapability::ShellExec);
        match self.role {
            AgentRole::Reviewer => {
                if write || shell {
                    return Err(Error::Validation {
                        field: "capabilities".into(),
                        message: "reviewer roles are read-only by policy".into(),
                    });
                }
            }
            AgentRole::Planner => {
                if write || shell {
                    return Err(Error::Validation {
                        field: "capabilities".into(),
                        message: "planner roles are read-only by policy".into(),
                    });
                }
            }
            AgentRole::Verifier => {
                if write {
                    return Err(Error::Validation {
                        field: "capabilities".into(),
                        message: "verifier roles must not mutate sources".into(),
                    });
                }
            }
            AgentRole::Implementer => {}
        }

        // 5) Shell grants must come with an explicit allowlist.
        if shell && self.allowed_commands.is_empty() {
            return Err(Error::Validation {
                field: "allowed_commands".into(),
                message: "shell capability requires a non-empty command allowlist".into(),
            });
        }

        // 6) Least privilege: every grant serves an allowed tool, and
        //    every allowed tool has its requirement granted.
        let mut required: Vec<ToolCapability> = Vec::new();
        for tool in &self.allowed_tools {
            if let Some(cap) = registry.required_capability(tool) {
                if !self.capabilities.contains(&cap) {
                    return Err(Error::Validation {
                        field: "capabilities".into(),
                        message: format!("tool {tool:?} needs capability {}", cap.as_str()),
                    });
                }
                required.push(cap);
            }
        }
        for cap in &self.capabilities {
            if !required.contains(cap) {
                return Err(Error::Validation {
                    field: "capabilities".into(),
                    message: format!(
                        "capability {} is not used by any allowed tool",
                        cap.as_str()
                    ),
                });
            }
        }

        // 7) Commands must be bare names; reject junk loudly instead of
        //    silently filtering it away. U+005C covers Windows-style
        //    separators on any host platform.
        for cmd in &self.allowed_commands {
            if cmd.is_empty()
                || cmd == "."
                || cmd == ".."
                || cmd.contains('/')
                || cmd.contains('\u{5c}')
            {
                return Err(Error::Validation {
                    field: "allowed_commands".into(),
                    message: format!("command {cmd:?} is not a bare executable name"),
                });
            }
        }

        Ok(())
    }

    /// Build the runtime capability set bound to one workspace root.
    ///
    /// Only call after [RoleManifest::validate] succeeded.
    pub fn capability_set(&self, workspace_root: std::path::PathBuf) -> CapabilitySet {
        let mut caps = CapabilitySet::default().with_workspace(workspace_root);
        for cap in &self.capabilities {
            caps = caps.with(*cap);
        }
        for cmd in &self.allowed_commands {
            caps = caps.with_command(cmd.clone());
        }
        caps
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn registry() -> ToolRegistry {
        ToolRegistry::with_builtins()
    }

    #[test]
    fn built_in_manifests_validate() {
        let reg = registry();
        for role in [
            AgentRole::Planner,
            AgentRole::Implementer,
            AgentRole::Reviewer,
            AgentRole::Verifier,
        ] {
            let m = RoleManifest::built_in(role);
            m.validate(&reg)
                .unwrap_or_else(|e| panic!("{role:?} manifest must validate: {e}"));
        }
    }

    #[test]
    fn reviewer_cannot_be_granted_write_or_shell() {
        let reg = registry();
        let mut m = RoleManifest::built_in(AgentRole::Reviewer);
        m.capabilities.push(ToolCapability::WorkspaceWrite);
        m.allowed_tools.push("fs.write".into());
        let err = m.validate(&reg).expect_err("write must be refused");
        assert!(err.to_string().contains("read-only"));

        let mut m2 = RoleManifest::built_in(AgentRole::Reviewer);
        m2.capabilities.push(ToolCapability::ShellExec);
        m2.allowed_tools.push("shell.exec".into());
        assert!(m2.validate(&reg).is_err());
    }

    #[test]
    fn planner_cannot_be_granted_write() {
        let reg = registry();
        let mut m = RoleManifest::built_in(AgentRole::Planner);
        m.capabilities.push(ToolCapability::WorkspaceWrite);
        m.allowed_tools.push("fs.write".into());
        assert!(m.validate(&reg).is_err());
    }

    #[test]
    fn verifier_cannot_be_granted_write() {
        let reg = registry();
        let mut m = RoleManifest::built_in(AgentRole::Verifier);
        m.capabilities.push(ToolCapability::WorkspaceWrite);
        m.allowed_tools.push("fs.write".into());
        let err = m.validate(&reg).expect_err("verifier write refused");
        assert!(err.to_string().contains("must not mutate"));
    }

    #[test]
    fn no_role_may_hold_administrative_capabilities() {
        let reg = registry();
        for role in [
            AgentRole::Planner,
            AgentRole::Implementer,
            AgentRole::Reviewer,
            AgentRole::Verifier,
        ] {
            let mut m = RoleManifest::built_in(role);
            m.capabilities.push(ToolCapability::SecretRead);
            assert!(
                m.validate(&reg).is_err(),
                "{role:?} must never hold SecretRead"
            );
        }
    }

    #[test]
    fn unknown_tool_rejected_at_validation_time() {
        let reg = registry();
        let mut m = RoleManifest::built_in(AgentRole::Planner);
        m.allowed_tools.push("db.drop_all".into());
        let err = m.validate(&reg).expect_err("unknown tool refused");
        assert!(err.to_string().contains("not registered"));
    }

    #[test]
    fn empty_manifest_is_a_configuration_error() {
        let reg = registry();
        let m = RoleManifest {
            role: AgentRole::Planner,
            description: "broken".into(),
            capabilities: Vec::new(),
            allowed_tools: Vec::new(),
            allowed_commands: Vec::new(),
        };
        assert!(m.validate(&reg).is_err());
    }

    #[test]
    fn dead_grants_are_rejected() {
        let reg = registry();
        // fs.write named without its WorkspaceWrite grant.
        let mut m = RoleManifest::built_in(AgentRole::Planner);
        m.allowed_tools.push("fs.write".into());
        assert!(m.validate(&reg).is_err());

        // And the inverse: a grant no remaining tool needs.
        let mut m2 = RoleManifest::built_in(AgentRole::Implementer);
        m2.allowed_tools.retain(|t| t != "shell.exec");
        let err = m2.validate(&reg).expect_err("ShellExec now unused");
        assert!(err.to_string().contains("not used by any allowed tool"));
    }

    #[test]
    fn shell_without_allowlist_rejected() {
        let reg = registry();
        let mut m = RoleManifest::built_in(AgentRole::Verifier);
        m.allowed_commands.clear();
        let err = m.validate(&reg).expect_err("empty allowlist refused");
        assert!(err.to_string().contains("non-empty command allowlist"));
    }

    #[test]
    fn non_bare_command_names_rejected() {
        let reg = registry();
        let mut m = RoleManifest::built_in(AgentRole::Verifier);
        m.allowed_commands.push("/bin/sh".into());
        let err = m.validate(&reg).expect_err("path-like command refused");
        assert!(err.to_string().contains("bare executable name"));
    }

    #[test]
    fn capability_set_binds_workspace_and_commands() {
        let m = RoleManifest::built_in(AgentRole::Verifier);
        let caps = m.capability_set(std::path::PathBuf::from("/tmp/ws"));
        assert!(caps.has(ToolCapability::WorkspaceRead));
        assert!(caps.has(ToolCapability::ShellExec));
        assert!(!caps.has(ToolCapability::WorkspaceWrite));
        assert!(caps.command_allowed("cargo"));
        assert!(!caps.command_allowed("curl"));
    }
}
