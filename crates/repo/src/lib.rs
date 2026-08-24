//! # hephaestus-repo
//!
//! Repository intelligence: safe git operations, tracked-file
//! inventory with language classification, and deterministic AST-based
//! symbol extraction (ADR-004).

use hephaestus_core::Result;

pub mod git;
pub mod inventory;
pub mod symbols;

pub use git::{CommitInfo, GitRepo};
pub use inventory::{FileEntry, Language};
pub use symbols::{Symbol, SymbolKind, SymbolRegistry};

/// Convenience: full snapshot analysis of a checkout.
///
/// Returns inventory plus extracted symbols per supported file. Files
/// that fail to read are skipped and counted - never silently treated
/// as empty.
pub fn analyze_checkout(repo: &GitRepo, registry: &SymbolRegistry) -> Result<CheckoutAnalysis> {
    let entries = inventory::scan_tracked(repo)?;
    let root = repo.workdir();
    let mut symbols = Vec::new();
    let mut unreadable = 0u64;

    for entry in &entries {
        let Some(lang) = entry.language else {
            continue;
        };
        if !registry.supports(lang) {
            continue;
        }
        match std::fs::read_to_string(root.join(&entry.path)) {
            Ok(source) => {
                for mut sym in registry.extract(lang, &source) {
                    sym.name = format!("{}::{}", entry.path, sym.name)
                        .chars()
                        .take(512)
                        .collect();
                    symbols.push(sym);
                }
            }
            Err(_) => {
                tracing::warn!(path = %entry.path, "unreadable source skipped");
                unreadable += 1;
            }
        }
    }

    Ok(CheckoutAnalysis {
        head_commit: repo.head_commit()?,
        branch: repo.current_branch()?,
        files: entries,
        symbols,
        unreadable_files: unreadable,
    })
}

/// Aggregate result of one checkout analysis.
#[derive(Debug, Clone)]
pub struct CheckoutAnalysis {
    /// HEAD hash at analysis time.
    pub head_commit: String,
    /// Branch name when attached.
    pub branch: Option<String>,
    /// Tracked-file inventory.
    pub files: Vec<FileEntry>,
    /// Extracted definitions across supported languages.
    pub symbols: Vec<Symbol>,
    /// Files present in the index but unreadable on disk.
    pub unreadable_files: u64,
}
