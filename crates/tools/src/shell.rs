//! Sandboxed shell execution.
//!
//! Isolation layers enforced here (ADR-006):
//! * argv-vector spawn, never a shell string
//! * command must be allowlisted by name in the caller's capabilities
//! * working directory locked to the capability's workspace root;
//!   relative paths resolve inside it, and the child inherits that cwd
//! * environment reduced to a minimal allowlist (no ambient secrets)
//! * wall-clock timeout kills runaway processes
//! * stdout/stderr capped; overflow truncates with a marker
//!
//! Network egress is governed by the NetworkEgress capability at the
//! tool layer: without it, network-dependent tools are un-invocable.
//! OS-level egress enforcement for arbitrary shell children is
//! platform-specific (seccomp/bubblewrap on Linux, sandbox profiles on
//! macOS) and lands with the hardened sandbox phase - tracked in the
//! threat model rather than claimed here.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use hephaestus_core::{Error, Result};

use crate::capabilities::{CapabilitySet, ToolCapability};

/// Result of one sandboxed command run.
#[derive(Debug, Clone)]
pub struct ShellOutcome {
    /// Process exit code when it ran to completion.
    pub exit_code: Option<i32>,
    /// Captured stdout (possibly truncated).
    pub stdout: String,
    /// Captured stderr (possibly truncated).
    pub stderr: String,
    /// True when output was truncated at the cap.
    pub truncated: bool,
    /// Wall-clock duration.
    pub duration: Duration,
    /// True when the timeout killed the process.
    pub timed_out: bool,
}

const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// Environment variables passed through to sandboxed children.
const ENV_ALLOWLIST: &[&str] = &["PATH", "LANG", "LC_ALL", "TERM"];

/// Sandboxed shell tool bound to a capability set.
pub struct SandboxedShell<'a> {
    caps: &'a CapabilitySet,
    timeout: Duration,
    max_output: usize,
}

impl<'a> SandboxedShell<'a> {
    /// Bind to the invoking agent's capabilities.
    pub fn new(caps: &'a CapabilitySet) -> Self {
        Self {
            caps,
            timeout: DEFAULT_TIMEOUT,
            max_output: MAX_OUTPUT,
        }
    }

    /// Override the wall-clock limit (still capped by config upstream).
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    /// Execute \`program\` with argument vectors inside the workspace.
    ///
    /// Fails closed on: missing ShellExec capability, unallowlisted
    /// program, no bound workspace, path-like program names.
    pub fn execute(&self, program: &str, args: &[&str]) -> Result<ShellOutcome> {
        // 1) Capability gate (outside any model/agent control).
        if !self.caps.has(ToolCapability::ShellExec) {
            return Err(Error::Forbidden {
                reason: "shell execution not granted to this agent".into(),
            });
        }
        // 2) Allowlist gate.
        if !self.caps.command_allowed(program) {
            return Err(Error::Forbidden {
                reason: format!("command {program:?} is not allowlisted"),
            });
        }
        // 3) Workspace gate.
        let Some(root) = self.caps.workspace_root() else {
            return Err(Error::Forbidden {
                reason: "no workspace bound to this capability set".into(),
            });
        };
        let root = absolutize(root)?;
        if !root.is_dir() {
            return Err(Error::Validation {
                field: "workspace".into(),
                message: "bound workspace root does not exist".into(),
            });
        }

        let started = std::time::Instant::now();
        let mut cmd = Command::new(program);
        cmd.current_dir(&root).args(args);
        apply_env_filter(&mut cmd);

        use wait_timeout::ChildExt;
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| Error::External {
                system: "shell",
                source: Box::new(e),
            })?;

        let status = match child.wait_timeout(self.timeout) {
            Ok(Some(s)) => s,
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(ShellOutcome {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::from("timeout exceeded"),
                    truncated: false,
                    duration: started.elapsed(),
                    timed_out: true,
                });
            }
            Err(e) => {
                return Err(Error::External {
                    system: "shell",
                    source: Box::new(e),
                });
            }
        };

        // Read after wait to avoid pipe deadlock; cap bytes.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(io) = child.stdout.take() {
            read_capped(io, &mut stdout, self.max_output);
        }
        if let Some(io) = child.stderr.take() {
            read_capped(io, &mut stderr, self.max_output);
        }
        let mut truncated = false;
        if stdout.len() >= self.max_output || stderr.len() >= self.max_output {
            truncated = true;
        }

        Ok(ShellOutcome {
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
            truncated,
            duration: started.elapsed(),
            timed_out: false,
        })
    }
}

fn absolutize(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .map_err(|e| Error::Storage(Box::new(e)))
    }
}

fn apply_env_filter(cmd: &mut Command) {
    // Start from an explicitly empty environment: nothing ambient
    // (tokens, AWS_*, KUBECONFIG...) leaks into tool children.
    cmd.env_clear();
    for key in ENV_ALLOWLIST {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    // Deterministic locale + non-interactive git for repo operations.
    cmd.env("LC_ALL", "C");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
}

fn read_capped<R: std::io::Read>(mut io: R, buf: &mut Vec<u8>, cap: usize) {
    use std::io::Read;
    let mut take = (&mut io).take(cap as u64);
    let _ = take.read_to_end(buf);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::path::PathBuf;

    fn caps_for(dir: PathBuf, cmds: &[&str]) -> CapabilitySet {
        let mut c = CapabilitySet::default()
            .with(ToolCapability::ShellExec)
            .with_workspace(dir);
        for n in cmds {
            c = c.with_command(*n);
        }
        c
    }

    #[test]
    fn runs_allowlisted_command_inside_workspace() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("probe.txt"), b"content").expect("w");
        let caps = caps_for(dir.path().to_path_buf(), &["ls"]);
        let shell = SandboxedShell::new(&caps);

        let out = shell.execute("ls", &[]).expect("run");
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.contains("probe.txt"), "cwd is the workspace");
        assert!(!out.timed_out);
    }

    #[test]
    fn denies_unlisted_and_uncapable_callers() {
        let dir = tempfile::tempdir().expect("tmp");
        // No ShellExec at all.
        let none = CapabilitySet::default().with_workspace(dir.path().to_path_buf());
        let err = SandboxedShell::new(&none).execute("ls", &[]).unwrap_err();
        assert!(matches!(err, Error::Forbidden { .. }));

        // Capability but command not allowlisted.
        let partial = caps_for(dir.path().to_path_buf(), &["git"]);
        let err2 = SandboxedShell::new(&partial)
            .execute("curl", &["-s"])
            .unwrap_err();
        assert!(matches!(err2, Error::Forbidden { reason } if reason.contains("allowlisted")));
    }

    #[test]
    fn env_filtered_no_ambient_secrets() {
        let dir = tempfile::tempdir().expect("tmp");
        let caps = caps_for(dir.path().to_path_buf(), &["env"]);
        let shell = SandboxedShell::new(&caps);
        let out = shell.execute("env", &[]).expect("run");
        // HOME is ambient but NOT on the allowlist: it must never
        // reach the sandboxed child, whatever its value is here.
        let home_leaked = out.stdout.lines().any(|l| l.starts_with("HOME="));
        assert!(!home_leaked, "non-allowlisted env var leaked");
        assert!(
            out.stdout.contains("LC_ALL=C"),
            "deterministic locale applied"
        );
    }

    #[test]
    fn timeout_kills_runaway_process() {
        let dir = tempfile::tempdir().expect("tmp");
        let caps = caps_for(dir.path().to_path_buf(), &["sleep"]);
        let shell = SandboxedShell::new(&caps).with_timeout(Duration::from_millis(150));
        let out = shell.execute("sleep", &["5"]).expect("run");
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
    }
}
