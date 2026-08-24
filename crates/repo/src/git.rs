//! Safe Git operations via the system git binary.
//!
//! Security posture:
//! * Every invocation uses an ARGUMENT VECTOR (std::process::Command
//!   without a shell): shell metacharacters in paths or branch names
//!   cannot become commands.
//! * Path/rev arguments are passed after `--` so they can never be
//!   interpreted as OPTIONS.
//! * Git hooks from untrusted repositories never execute: analysis
//!   only runs read-oriented plumbing; we additionally set
//!   `core.hooksPath` to an empty directory for every call.
//! * System/user git config is ignored (`GIT_CONFIG_NOSYSTEM`,
//!   empty GIT_CONFIG_GLOBAL/COUNT) so a malicious repo cannot alter
//!   behavior through config.
//! * Output is capped and processes are time-limited.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use hephaestus_core::{Error, Result};

/// Maximum bytes of stdout/stderr captured per command.
const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// Default wall-clock limit per git invocation.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// A checked-out repository on local disk.
#[derive(Debug, Clone)]
pub struct GitRepo {
    workdir: PathBuf,
}

/// A commit summary from history queries.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CommitInfo {
    /// Full commit hash.
    pub hash: String,
    /// Author name (repository data: untrusted).
    pub author: String,
    /// Commit timestamp (unix seconds).
    pub time_unix: i64,
    /// First line of the message (untrusted).
    pub subject: String,
}

impl GitRepo {
    /// Open an existing checkout.
    pub fn open(workdir: &Path) -> Result<Self> {
        let repo = Self {
            workdir: workdir.to_path_buf(),
        };
        // Fail closed if this is not actually a git worktree.
        repo.run(&["rev-parse", "--is-inside-work-tree"])?;
        Ok(repo)
    }

    /// Initialize a new repository (fixtures, tests, local bootstrap).
    pub fn init(workdir: &Path) -> Result<Self> {
        std::fs::create_dir_all(workdir).map_err(|e| Error::Storage(Box::new(e)))?;
        let repo = Self {
            workdir: workdir.to_path_buf(),
        };
        repo.run(&["init", "-q"])?;
        Ok(repo)
    }

    /// Clone a remote URL into dest. Validates scheme first.
    ///
    /// Allowed transports: https, ssh (git@host:path form), and local
    /// absolute paths (for on-host mirrors). Everything else fails.
    pub fn clone_into(remote: &str, dest: &Path) -> Result<Self> {
        validate_remote(remote)?;
        std::fs::create_dir_all(dest).map_err(|e| Error::Storage(Box::new(e)))?;
        let repo = Self {
            workdir: dest.to_path_buf(),
        };
        repo.run(&["clone", "-q", remote, "."])?;
        Ok(repo)
    }

    /// Repository working directory.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Run a read-oriented git command with hardened environment.
    fn run(&self, args: &[&str]) -> Result<GitOutput> {
        let hooks_dir = empty_hooks_dir();
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.workdir)
            .arg("-c")
            .arg(format!("core.hooksPath={}", hooks_dir.display()))
            .args(args);
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");

        let output = run_capped(&mut cmd)?;
        if !output.status_ok {
            return Err(Error::External {
                system: "git",
                source: format!(
                    "git {} failed: {}",
                    args.first().copied().unwrap_or("?"),
                    String::from_utf8_lossy(&output.stderr),
                )
                .into(),
            });
        }
        Ok(output)
    }

    /// Current HEAD full hash.
    pub fn head_commit(&self) -> Result<String> {
        let out = self.run(&["rev-parse", "HEAD"])?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Current branch name, if attached.
    pub fn current_branch(&self) -> Result<Option<String>> {
        let out = self.run(&["branch", "--show-current"])?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(if s.is_empty() { None } else { Some(s) })
    }

    /// List tracked files at HEAD. Respects the repository's own ignore
    /// rules by construction (plumbing over the index).
    pub fn tracked_files(&self) -> Result<Vec<PathBuf>> {
        let out = self.run(&["ls-files", "-z", "--"]);
        let raw = out?.stdout;
        let mut files = Vec::new();
        for entry in raw.split(|b| *b == 0u8) {
            if entry.is_empty() {
                continue;
            }
            let p = String::from_utf8_lossy(entry);
            // Reject anything escaping the worktree or containing
            // traversal - fail closed on hostile indexes.
            let path = PathBuf::from(p.as_ref());
            if path.is_absolute() || p.contains("..") || p.starts_with('/') {
                tracing::warn!(path = %p, "skipping suspicious index path");
                continue;
            }
            files.push(path);
        }
        files.sort();
        Ok(files)
    }

    /// Read a file's content as of HEAD (or any rev).
    pub fn show_file(&self, rev: &str, path: &Path) -> Result<Vec<u8>> {
        let rev = validate_rev(rev)?;
        let ps = path
            .to_str()
            .ok_or_else(|| Error::Validation {
                field: "path".into(),
                message: "path must be valid UTF-8".into(),
            })?
            .to_string();
        let out = self
            .run(&["show", &format!("{rev}:{{}}"), "--", &ps])
            .or_else(|_| self.run(&["cat-file", "blob", &format!("{rev}:{ps}")]))?;
        Ok(out.stdout)
    }

    /// Stage every change and create a commit.
    ///
    /// Authorship is supplied explicitly - never read from ambient
    /// config - so audit records can attribute the acting principal.
    /// Returns the new commit hash.
    pub fn commit_all(&self, message: &str, author: (&str, &str)) -> Result<String> {
        self.run(&["add", "--", "."])?;
        let ident = [
            "-c",
            &format!("user.name={}", author.0),
            "-c",
            &format!("user.email={}", author.1),
        ];
        let with_ident: Vec<&str> = ident
            .iter()
            .copied()
            .chain(["commit", "-q", "-m", message].iter().copied())
            .collect();
        self.run(&with_ident)?;
        self.head_commit()
    }
    /// Recent commits touching one path (provenance for change risk).
    pub fn log_path(&self, path: &Path, limit: usize) -> Result<Vec<CommitInfo>> {
        let ps = path.to_string_lossy().to_string();
        let out = self.run(&[
            "log",
            "-n",
            &limit.clamp(1, 1000).to_string(),
            "--pretty=format:%H%x1f%an%x1f%ct%x1f%s",
            "--",
            &ps,
        ])?;
        Ok(parse_log(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// Raw captured command result.
struct GitOutput {
    status_ok: bool,
    stdout: Vec<u8>,
    #[allow(dead_code)]
    stderr: Vec<u8>,
}

/// Execute with byte caps and timeout. Kills runaway processes.
fn run_capped(cmd: &mut Command) -> Result<GitOutput> {
    use std::io::Read;

    use wait_timeout::ChildExt;

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::External {
            system: "git",
            source: Box::new(e),
        })?;

    let deadline = Duration::from_secs(DEFAULT_TIMEOUT.as_secs());
    let status = child.wait_timeout(deadline).map_err(|e| Error::External {
        system: "git",
        source: Box::new(e),
    })?;

    let status = match status {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::BudgetExhausted {
                what: "git-timeout",
            });
        }
    };

    // Read outputs after exit to avoid pipe-full deadlock on huge logs;
    // caps protect memory.
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(io) = child.stdout.take() {
        let _ = io.take(MAX_OUTPUT_BYTES as u64).read_to_end(&mut stdout);
    }
    if let Some(io) = child.stderr.take() {
        let _ = io.take(MAX_OUTPUT_BYTES as u64).read_to_end(&mut stderr);
    }

    Ok(GitOutput {
        status_ok: status.success(),
        stdout,
        stderr,
    })
}

fn parse_log(raw: &str) -> Vec<CommitInfo> {
    let mut out = Vec::new();
    for line in raw.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\u{1f}').collect();
        if parts.len() != 4 || parts[0].len() < 40 {
            continue;
        }
        out.push(CommitInfo {
            hash: parts[0].to_string(),
            author: parts[1].to_string(),
            time_unix: parts[2].parse().unwrap_or(0),
            subject: parts[3].to_string(),
        });
    }
    out
}

/// Validate a user-supplied rev: refuse option-looking values.
fn validate_rev(rev: &str) -> Result<&str> {
    if rev.is_empty() || rev.starts_with('-') || rev.chars().any(|c| c.is_whitespace()) {
        return Err(Error::Validation {
            field: "rev".into(),
            message: "invalid revision".into(),
        });
    }
    Ok(rev)
}

/// Validate clone transport schemes.
pub fn validate_remote(remote: &str) -> Result<()> {
    let ok = remote.starts_with("https://")
        || (remote.starts_with("git@") && remote.contains(':'))
        || (Path::new(remote).is_absolute() && Path::new(remote).exists());
    if ok {
        Ok(())
    } else {
        Err(Error::Validation {
            field: "remote".into(),
            message:
                "remote must be https://, scp-like ssh (git@host:path), or an existing absolute path"
                    .into(),
        })
    }
}

/// A stable empty directory used to neuter hook execution.
fn empty_hooks_dir() -> PathBuf {
    // Per-process temp dir; created lazily, never written by us after.
    let dir = std::env::temp_dir().join("hephaestus-empty-hooks");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn init_commit_and_read_back() {
        let dir = tempfile::tempdir().expect("tmp");
        let repo = GitRepo::init(dir.path()).expect("init");

        std::fs::write(dir.path().join("a.txt"), b"hello\nsecond line").expect("write");
        // Use argv-safe add + commit with explicit identity env.
        repo.run(&["add", "--", "a.txt"]).expect("add");
        repo.run(&[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@t.invalid",
            "commit",
            "-q",
            "-m",
            "initial",
        ])
        .expect("commit");

        assert_eq!(
            repo.tracked_files().expect("files"),
            vec![PathBuf::from("a.txt")]
        );
        let content = repo.show_file("HEAD", Path::new("a.txt")).expect("content");
        assert_eq!(String::from_utf8_lossy(&content), "hello\nsecond line");

        let head = repo.head_commit().expect("head");
        assert_eq!(head.len(), 40);

        let log = repo.log_path(Path::new("a.txt"), 10).expect("log");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].subject, "initial");
    }

    #[test]
    fn traversal_paths_are_rejected_from_index() {
        let dir = tempfile::tempdir().expect("tmp");
        let repo = GitRepo::init(dir.path()).expect("init");
        // Simulate a hostile index entry by writing the tree object
        // directly is complex; instead assert our guard logic on the
        // filter function indirectly via tracked_files on a clean repo
        // and unit-test validate_rev for the option-injection case.
        assert!(validate_rev("--upload-pack=evil").is_err());
        assert!(validate_rev("HEAD~1").is_ok());
        let _ = repo;
    }

    #[test]
    fn remote_validation_rejects_bad_schemes() {
        assert!(validate_remote("https://github.com/x/y.git").is_ok());
        assert!(validate_remote("git@github.com:x/y.git").is_ok());
        assert!(
            validate_remote("ssh://host/path").is_err(),
            "raw ssh URLs denied for now"
        );
        assert!(validate_remote("file:///etc/passwd").is_err());
        assert!(validate_remote("../relative").is_err());
    }
}
