//! Folder A vs Folder B comparison (inspired by compare_folders.py).
//!
//! We compare by *relative path* (not content-hash) because the tool's job is
//! "what exists in one tree but not the mirrored spot in the other" — a sync
//! gap finder, not a duplicate-content finder (that's the Find Duplicates tab).

use crate::fs_utils::{io_err, transfer_file};
use crate::TransferMode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the root that was scanned (A or B) — this is the key
    /// used to match "the same file" across both trees.
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComparisonResult {
    pub only_in_a: Vec<FileEntry>,
    pub only_in_b: Vec<FileEntry>,
}

fn scan_tree(root: &Path) -> crate::Result<BTreeMap<String, FileEntry>> {
    if !root.is_dir() {
        return Err(crate::FtpError::InvalidDir(root.to_path_buf()));
    }
    let mut map = BTreeMap::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(root)
            .unwrap_or(&abs)
            .to_string_lossy()
            .replace('\\', "/");
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        map.insert(rel.clone(), FileEntry { relative_path: rel, absolute_path: abs, size });
    }
    Ok(map)
}

/// Compares two directory trees by relative path and returns what's unique
/// to each side. O(n) after the two directory walks (BTreeMap diff).
pub fn compare_folders(folder_a: &Path, folder_b: &Path) -> crate::Result<ComparisonResult> {
    let a = scan_tree(folder_a)?;
    let b = scan_tree(folder_b)?;

    let only_in_a = a
        .iter()
        .filter(|(k, _)| !b.contains_key(*k))
        .map(|(_, v)| v.clone())
        .collect();
    let only_in_b = b
        .iter()
        .filter(|(k, _)| !a.contains_key(*k))
        .map(|(_, v)| v.clone())
        .collect();

    Ok(ComparisonResult { only_in_a, only_in_b })
}

/// One conditional rule for the "Auto update" advanced menu, e.g.
/// "if the relative path contains `Admin/Entreprise`, cut instead of copy".
/// Rules are evaluated top-to-bottom; the first match wins. If nothing
/// matches, `default_mode` is used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoUpdateRule {
    /// Case-insensitive substring match against the file's relative path.
    pub path_contains: String,
    pub mode: TransferMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoUpdateConfig {
    pub rules: Vec<AutoUpdateRule>,
    pub default_mode: TransferMode,
}

impl Default for AutoUpdateConfig {
    fn default() -> Self {
        Self { rules: Vec::new(), default_mode: TransferMode::Copy }
    }
}

fn resolve_mode(relative_path: &str, config: &AutoUpdateConfig) -> TransferMode {
    let lower = relative_path.to_lowercase();
    for rule in &config.rules {
        if lower.contains(&rule.path_contains.to_lowercase()) {
            return rule.mode;
        }
    }
    config.default_mode
}

/// Mirrors a single file across to the other tree, preserving its relative
/// path (so "same arborescence in the other folder" holds). `source_root`
/// is the root the entry came from (A or B); `dest_root` is the other one.
pub fn auto_update_one(
    entry: &FileEntry,
    dest_root: &Path,
    config: &AutoUpdateConfig,
) -> crate::Result<PathBuf> {
    let mode = resolve_mode(&entry.relative_path, config);
    let dest = dest_root.join(&entry.relative_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    transfer_file(&entry.absolute_path, &dest, mode)
}

/// Batch version for the multi-select "apply to all selected rows" UI action.
pub fn auto_update_many(
    entries: &[FileEntry],
    dest_root: &Path,
    config: &AutoUpdateConfig,
    mut on_progress: impl FnMut(usize, usize),
) -> Vec<(String, crate::Result<PathBuf>)> {
    let total = entries.len();
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let result = auto_update_one(e, dest_root, config);
            on_progress(i + 1, total);
            (e.relative_path.clone(), result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, content: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn finds_files_unique_to_each_side() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A");
        let b = dir.path().join("B");
        touch(&a.join("shared.txt"), b"x");
        touch(&b.join("shared.txt"), b"x");
        touch(&a.join("only_a.txt"), b"y");
        touch(&b.join("nested/only_b.txt"), b"z");

        let result = compare_folders(&a, &b).unwrap();
        assert_eq!(result.only_in_a.len(), 1);
        assert_eq!(result.only_in_a[0].relative_path, "only_a.txt");
        assert_eq!(result.only_in_b.len(), 1);
        assert_eq!(result.only_in_b[0].relative_path, "nested/only_b.txt");
    }

    #[test]
    fn auto_update_rule_overrides_default_mode() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A");
        let b = dir.path().join("B");
        touch(&a.join("Admin/Entreprise/report.txt"), b"data");
        std::fs::create_dir_all(&b).unwrap();

        let result = compare_folders(&a, &b).unwrap();
        let entry = &result.only_in_a[0];

        let config = AutoUpdateConfig {
            rules: vec![AutoUpdateRule {
                path_contains: "Admin/Entreprise".into(),
                mode: TransferMode::Cut,
            }],
            default_mode: TransferMode::Copy,
        };
        assert_eq!(resolve_mode(&entry.relative_path, &config), TransferMode::Cut);

        auto_update_one(entry, &b, &config).unwrap();
        // Cut mode: source should be gone, dest should exist.
        assert!(!entry.absolute_path.exists());
        assert!(b.join("Admin/Entreprise/report.txt").exists());
    }
}
