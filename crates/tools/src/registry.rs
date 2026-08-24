//! Tool registry: what exists, and which capability it requires.

use std::collections::HashMap;

use crate::capabilities::ToolCapability;

/// Static description of one tool.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Canonical tool name (snake_case, unique in the registry).
    pub name: &'static str,
    /// One-line description for agent prompts and docs.
    pub description: &'static str,
    /// Capability required to invoke this tool at all.
    pub required: ToolCapability,
}

/// Registry of known tools.
///
/// Builtins are installed once at startup; MCP-bridged tools register
/// through the same path so they inherit identical governance.
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, ToolDefinition>,
}

impl ToolRegistry {
    /// Registry with the builtin tool set.
    pub fn with_builtins() -> Self {
        let mut reg = Self::default();
        for def in builtin_tools() {
            reg.tools.insert(def.name, def);
        }
        reg
    }

    /// Look up a definition by name.
    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// All registered definitions (sorted by name for stable output).
    pub fn list(&self) -> Vec<&ToolDefinition> {
        let mut v: Vec<_> = self.tools.values().collect();
        v.sort_by_key(|d| d.name);
        v
    }

    /// The capability a tool requires, if registered.
    pub fn required_capability(&self, name: &str) -> Option<ToolCapability> {
        self.tools.get(name).map(|d| d.required)
    }
}

/// The current builtin tool set.
pub fn builtin_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "shell.exec",
            description: "Run an allowlisted command inside the workspace sandbox",
            required: ToolCapability::ShellExec,
        },
        ToolDefinition {
            name: "fs.read",
            description: "Read a file inside the assigned workspace",
            required: ToolCapability::WorkspaceRead,
        },
        ToolDefinition {
            name: "fs.write",
            description: "Write a file inside the assigned workspace",
            required: ToolCapability::WorkspaceWrite,
        },
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn builtins_registered_with_unique_names() {
        let reg = ToolRegistry::with_builtins();
        let list = reg.list();
        assert!(list.len() >= 3);
        let names: Vec<_> = list.iter().map(|d| d.name).collect();
        let uniq: std::collections::HashSet<_> = names.clone().into_iter().collect();
        assert_eq!(names.len(), uniq.len(), "duplicate tool names");
    }

    #[test]
    fn shell_exec_requires_shell_capability() {
        let reg = ToolRegistry::with_builtins();
        assert_eq!(
            reg.required_capability("shell.exec"),
            Some(ToolCapability::ShellExec)
        );
        assert!(reg.required_capability("nonexistent").is_none());
    }
}
