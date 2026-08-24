//! Workspace filesystem tools.
//!
//! Path containment is enforced by canonicalization: every resolved
//! path must remain inside the capability's workspace root. Symlink
//! escapes therefore fail closed (canonicalize resolves them before
//! the prefix check).

use std::path::{Path, PathBuf};

use hephaestus_core::{Error, Result};

use crate::capabilities::{CapabilitySet, ToolCapability};

/// Maximum bytes read or written per call.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Resolve `relative` under `root`, rejecting escapes.
///
/// Accepts relative paths only; absolute input fails validation so a
/// hostile task can never steer reads outside even with capability.
fn contained(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.starts_with('\\')
        || Path::new(relative).is_absolute()
    {
        return Err(Error::Validation {
            field: "path".into(),
            message: "workspace paths must be relative".into(),
        });
    }
    let candidate = root.join(relative);
    let canon = candidate
        .canonicalize()
        .or_else(|_| {
            // Target may not exist yet (write path): canonicalize the
            // deepest existing ancestor instead.
            let mut anc = candidate.clone();
            while !anc.exists() {
                anc = match anc.parent() {
                    Some(p) => p.to_path_buf(),
                    None => break,
                };
                if anc.as_os_str().is_empty() {
                    break;
                }
            }
            anc.canonicalize().map(|a| {
                // Rejoin any missing tail onto the resolved ancestor.
                a.join(
                    candidate
                        .strip_prefix(&anc)
                        .unwrap_or_else(|_| Path::new("")),
                )
            })
        })
        .map_err(|_| Error::Validation {
            field: "path".into(),
            message: "path does not resolve inside the workspace".into(),
        })?;

    let root_canon = root
        .canonicalize()
        .map_err(|e| Error::Storage(Box::new(e)))?;
    if canon.starts_with(&root_canon) {
        Ok(canon)
    } else {
        Err(Error::Forbidden {
            reason: "path escapes the assigned workspace".into(),
        })
    }
}

/// Read a file inside the workspace.
pub fn fs_read(caps: &CapabilitySet, relative: &str) -> Result<Vec<u8>> {
    if !caps.has(ToolCapability::WorkspaceRead) {
        return Err(Error::Forbidden {
            reason: "workspace read not granted".into(),
        });
    }
    let root = caps.workspace_root().ok_or(Error::Forbidden {
        reason: "no workspace bound".into(),
    })?;
    let path = contained(root, relative)?;
    let meta = std::fs::metadata(&path).map_err(|e| Error::Storage(Box::new(e)))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(Error::BudgetExhausted { what: "file-size" });
    }
    std::fs::read(&path).map_err(|e| Error::Storage(Box::new(e)))
}

/// Write a file inside the workspace (creates parents).
pub fn fs_write(caps: &CapabilitySet, relative: &str, bytes: &[u8]) -> Result<u64> {
    if !caps.has(ToolCapability::WorkspaceWrite) {
        return Err(Error::Forbidden {
            reason: "workspace write not granted".into(),
        });
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::BudgetExhausted { what: "file-size" });
    }
    let root = caps.workspace_root().ok_or(Error::Forbidden {
        reason: "no workspace bound".into(),
    })?;
    let path = contained(root, relative)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Storage(Box::new(e)))?;
    }
    let n = bytes.len() as u64;
    std::fs::write(&path, bytes).map_err(|e| Error::Storage(Box::new(e)))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn caps(dir: &Path, write: bool) -> CapabilitySet {
        let c = CapabilitySet::default()
            .with(ToolCapability::WorkspaceRead)
            .with_workspace(dir.to_path_buf());
        if write {
            c.with(ToolCapability::WorkspaceWrite)
        } else {
            c
        }
    }

    #[test]
    fn read_write_roundtrip_inside_workspace() {
        let dir = tempfile::tempdir().expect("tmp");
        let c = caps(dir.path(), true);
        let n = fs_write(&c, "src/main.rs", b"fn main() {}").expect("write");
        assert_eq!(n, b"fn main() {}".len() as u64);
        let got = fs_read(&c, "src/main.rs").expect("read");
        assert_eq!(got, b"fn main() {}");
    }

    #[test]
    fn symlink_escape_fails_closed() {
        let dir = tempfile::tempdir().expect("tmp");
        let outside = tempfile::tempdir().expect("out");
        std::fs::write(outside.path().join("secret"), b"top secret").expect("w");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("symlink");
        let c = caps(dir.path(), true);
        let err = fs_read(&c, "link/secret").unwrap_err();
        assert!(matches!(err, Error::Forbidden { .. }), "got {err:?}");
    }

    #[test]
    fn traversal_and_absolute_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        let c = caps(dir.path(), true);
        for bad in ["../escape.txt", "/etc/passwd", ""] {
            assert!(fs_write(&c, bad, b"x").is_err(), "{bad:?} must fail");
        }
    }

    #[test]
    fn read_capability_required_for_reads() {
        let dir = tempfile::tempdir().expect("tmp");
        let none = CapabilitySet::default().with_workspace(dir.path().to_path_buf());
        assert!(fs_read(&none, "x.txt").is_err());
    }
}
