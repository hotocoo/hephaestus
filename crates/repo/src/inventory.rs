//! File inventory and language classification.
//!
//! Inventory is derived from the git index (tracked files only) so
//! ignore rules are honored by construction and untracked junk (or
//! attacker-planted files) never enters analysis.

use std::path::Path;

use serde::Serialize;

/// Languages with first-class support.
///
/// Additional languages slot in by adding a classifier entry plus a
/// symbol extractor; core logic never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Rust sources.
    Rust,
    /// TypeScript.
    TypeScript,
    /// TSX (typed React).
    Tsx,
    /// JavaScript.
    JavaScript,
    /// Python.
    Python,
    /// Go.
    Go,
    /// Java.
    Java,
}

impl Language {
    /// Map from file extension. Compound extensions checked first.
    pub fn from_path(path: &Path) -> Option<Language> {
        let name = path.file_name()?.to_str()?;
        if name.ends_with(".tsx") {
            return Some(Language::Tsx);
        }
        if name.ends_with(".d.ts") {
            return None; // ambient declarations: skip analysis
        }
        let ext = path.extension()?.to_str()?;
        match ext {
            "rs" => Some(Language::Rust),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "py" => Some(Language::Python),
            "go" => Some(Language::Go),
            "java" => Some(Language::Java),
            _ => None,
        }
    }

    /// Stable name used in storage and API responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Java => "java",
        }
    }
}

/// One tracked file with classification metadata.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileEntry {
    /// Workspace-relative path.
    pub path: String,
    /// Size on disk in bytes (HEAD content size when snapshotted).
    pub size_bytes: u64,
    /// Detected language, when supported.
    pub language: Option<Language>,
}

/// Build an inventory from a checkout using its git index.
pub fn scan_tracked(repo: &crate::git::GitRepo) -> hephaestus_core::Result<Vec<FileEntry>> {
    let root = repo.workdir();
    let mut entries = Vec::new();
    for path in repo.tracked_files()? {
        let full = root.join(&path);
        let meta = std::fs::metadata(&full).ok();
        entries.push(FileEntry {
            path: path.to_string_lossy().to_string(),
            size_bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            language: Language::from_path(&path),
        });
    }
    Ok(entries)
}

/// Summary counts per language for dashboards and routing decisions.
pub fn summarize(entries: &[FileEntry]) -> Vec<(String, usize)> {
    let mut counts = std::collections::BTreeMap::new();
    for e in entries {
        if let Some(lang) = e.language {
            *counts.entry(lang.as_str().to_string()).or_insert(0usize) += 1;
        }
    }
    counts.into_iter().collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn classify_common_extensions() {
        assert_eq!(
            Language::from_path(Path::new("src/main.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path(Path::new("web/app.tsx")),
            Some(Language::Tsx)
        );
        assert_eq!(
            Language::from_path(Path::new("web/app.ts")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_path(Path::new("x/py.py")),
            Some(Language::Python)
        );
        assert_eq!(
            Language::from_path(Path::new("m/main.go")),
            Some(Language::Go)
        );
        assert_eq!(
            Language::from_path(Path::new("A.java")),
            Some(Language::Java)
        );
        assert_eq!(Language::from_path(Path::new("types.d.ts")), None);
        assert_eq!(Language::from_path(Path::new("Makefile")), None);
    }

    #[test]
    fn inventory_uses_git_index() {
        let dir = tempfile::tempdir().expect("tmp");
        let repo = crate::git::GitRepo::init(dir.path()).expect("init");
        std::fs::write(dir.path().join("lib.rs"), b"fn main() {}").expect("w");
        std::fs::create_dir(dir.path().join("sub")).expect("d");
        std::fs::write(
            dir.path().join("sub/app.py"),
            b"def f():
    pass
",
        )
        .expect("w2");
        // Ignored via repo rules BEFORE staging: proves inventory
        // follows the index (which honors ignore files).
        std::fs::write(dir.path().join(".gitignore"), b"notes.txt\n").expect("gi");
        std::fs::write(dir.path().join("notes.txt"), b"ignored ext").expect("w3");

        repo.commit_all("x", ("T", "t@t.invalid")).expect("commit");

        let inv = scan_tracked(&repo).expect("scan");
        // .gitignore + lib.rs + sub/app.py; notes.txt stays ignored.
        assert_eq!(inv.len(), 3, "ignored files must not enter inventory");
        let paths: Vec<_> = inv.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&"lib.rs".to_string()));
        assert!(!paths.iter().any(|p| p.contains("notes.txt")));
        assert_eq!(
            summarize(&inv),
            vec![("python".into(), 1), ("rust".into(), 1)]
        );
    }
}
