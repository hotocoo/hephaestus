//! Agent capability sets.
//!
//! Deny by default. A capability not present in the set makes the
//! corresponding tool un-invocable - the runtime refuses before the
//! tool body ever runs, and the refusal is audited.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Individual grantable capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCapability {
    /// Read files inside the assigned workspace.
    WorkspaceRead,
    /// Write files inside the assigned workspace.
    WorkspaceWrite,
    /// Execute allowlisted shell commands in the workspace sandbox.
    ShellExec,
    /// Reach the network from tool executions (rare; policy-gated).
    NetworkEgress,
    /// Read secret material (administrative roles only).
    SecretRead,
    /// Trigger deployments to environments.
    Deploy,
}

impl ToolCapability {
    /// Stable string used in audit entries and API payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolCapability::WorkspaceRead => "workspace_read",
            ToolCapability::WorkspaceWrite => "workspace_write",
            ToolCapability::ShellExec => "shell_exec",
            ToolCapability::NetworkEgress => "network_egress",
            ToolCapability::SecretRead => "secret_read",
            ToolCapability::Deploy => "deploy",
        }
    }
}

/// The set of capabilities granted to one agent run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySet {
    grants: HashSet<ToolCapability>,
    /// Workspace root this agent may touch (None = no fs access).
    workspace_root: Option<std::path::PathBuf>,
    /// Allowlisted command names for ShellExec.
    allowed_commands: Vec<String>,
}

impl Default for CapabilitySet {
    // Empty set: deny by default. Kept explicit (not derived) so the
    // security posture is stated where a reader looks first.
    #[allow(clippy::derivable_impls)]
    fn default() -> Self {
        Self {
            grants: HashSet::new(),
            workspace_root: None,
            allowed_commands: Vec::new(),
        }
    }
}

impl CapabilitySet {
    /// Builder-style grant.
    pub fn with(mut self, cap: ToolCapability) -> Self {
        self.grants.insert(cap);
        self
    }

    /// Bind a workspace root (required for any filesystem capability
    /// to be meaningful).
    pub fn with_workspace(mut self, root: std::path::PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Add an allowlisted executable name for shell execution.
    pub fn with_command(mut self, name: impl Into<String>) -> Self {
        let n = name.into();
        // Only bare names are allowlisted: absolute paths or anything
        // containing separators is rejected outright.
        if !n.is_empty() && !n.contains('/') && !n.contains('\\') && n != "." && n != ".." {
            self.allowed_commands.push(n);
        }
        self
    }

    /// Check a capability. Fails closed when absent.
    pub fn has(&self, cap: ToolCapability) -> bool {
        self.grants.contains(&cap)
    }

    /// Workspace root when bound.
    pub fn workspace_root(&self) -> Option<&std::path::PathBuf> {
        self.workspace_root.as_ref()
    }

    /// Whether an exact command name is allowlisted.
    pub fn command_allowed(&self, name: &str) -> bool {
        self.allowed_commands.iter().any(|a| a == name)
    }

    /// Names of allowlisted commands (for diagnostics).
    pub fn allowed_commands(&self) -> &[String] {
        &self.allowed_commands
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn default_denies_everything() {
        let caps = CapabilitySet::default();
        assert!(!caps.has(ToolCapability::WorkspaceRead));
        assert!(!caps.has(ToolCapability::ShellExec));
        assert!(!caps.has(ToolCapability::Deploy));
        assert!(caps.workspace_root().is_none());
        assert!(!caps.command_allowed("git"));
    }

    #[test]
    fn grants_accumulate_and_check() {
        let caps = CapabilitySet::default()
            .with(ToolCapability::WorkspaceRead)
            .with(ToolCapability::WorkspaceWrite)
            .with(ToolCapability::ShellExec)
            .with_workspace(std::path::PathBuf::from("/tmp/ws"))
            .with_command("git")
            .with_command("cargo");

        assert!(caps.has(ToolCapability::WorkspaceRead));
        assert!(caps.has(ToolCapability::WorkspaceWrite));
        assert!(!caps.has(ToolCapability::NetworkEgress), "not granted");
        assert!(caps.command_allowed("git"));
        assert!(caps.command_allowed("cargo"));
        assert!(!caps.command_allowed("curl"), "unlisted commands denied");
    }

    #[test]
    fn path_like_command_names_rejected_from_allowlist() {
        let caps = CapabilitySet::default()
            .with_command("/bin/sh")
            .with_command("../evil")
            .with_command("ok");
        assert!(!caps.command_allowed("/bin/sh"));
        assert!(!caps.command_allowed("../evil"));
        assert!(caps.command_allowed("ok"));
    }
}
