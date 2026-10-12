//! Content-based duplicate finder (ported from find_duplicates.py).
//!
//! Two-phase strategy, same as the Python original and for the same reason —
//! hashing is the expensive part, so we only hash files that already share a
//! size with at least one other file:
//!   1. Group by file size (cheap: one `stat` per file).
//!   2. SHA-256 only within same-size groups, then group by (size, hash).

use crate::hash::sha256_file;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub sha256: String,
    pub size: u64,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateScanOptions {
    pub min_size: u64,
    pub ignore_empty: bool,
    /// Advanced menu: restrict to these extensions (lowercase, no dot). Empty = all.
    pub extensions: Vec<String>,
    /// Advanced menu: skip files modified more than N days ago. `None` = no limit.
    pub modified_within_days: Option<u32>,
}

impl Default for DuplicateScanOptions {
    fn default() -> Self {
        Self { min_size: 0, ignore_empty: false, extensions: Vec::new(), modified_within_days: None }
    }
}

fn passes_filters(path: &Path, size: u64, opts: &DuplicateScanOptions) -> bool {
    if size < opts.min_size {
        return false;
    }
    if opts.ignore_empty && size == 0 {
        return false;
    }
    if !opts.extensions.is_empty() {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !opts.extensions.iter().any(|e| e.to_lowercase() == ext) {
            return false;
        }
    }
    if let Some(days) = opts.modified_within_days {
        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                let age = std::time::SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or_default();
                if age.as_secs() > (days as u64) * 86_400 {
                    return false;
                }
            }
        }
    }
    true
}

/// Scans one or more root folders and returns groups of byte-identical files.
/// `on_progress(done, total)` is called throughout: during the initial walk
/// `total` is `0` (still counting — the UI should show an indeterminate
/// state), then once candidates are known `total` becomes their count and
/// `done` climbs towards it as hashing proceeds.
pub fn find_duplicates(
    roots: &[PathBuf],
    opts: &DuplicateScanOptions,
    mut on_progress: impl FnMut(usize, usize),
) -> crate::Result<Vec<DuplicateGroup>> {
    // Phase 1: group by size.
    let mut size_map: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let mut scanned = 0usize;

    for root in roots {
        if !root.is_dir() {
            return Err(crate::FtpError::InvalidDir(root.clone()));
        }
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().to_path_buf();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if !passes_filters(&path, size, opts) {
                continue;
            }
            size_map.entry(size).or_default().push(path);
            scanned += 1;
            if scanned % 100 == 0 {
                on_progress(scanned, 0);
            }
        }
    }

    let candidates: Vec<PathBuf> = size_map
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();

    // Phase 2: hash only the candidates, in chunks — each chunk is hashed in
    // parallel (rayon) for speed, but we report progress between chunks from
    // the main thread so the UI gets a real, steadily climbing percentage.
    const CHUNK: usize = 64;
    let total = candidates.len();
    on_progress(0, total);
    let mut hashed: Vec<(u64, String, PathBuf)> = Vec::with_capacity(total);
    let mut done = 0usize;
    for chunk in candidates.chunks(CHUNK) {
        let chunk_hashed: Vec<(u64, String, PathBuf)> = chunk
            .par_iter()
            .filter_map(|path| {
                let size = std::fs::metadata(path).ok()?.len();
                let digest = sha256_file(path).ok()?;
                Some((size, digest, path.clone()))
            })
            .collect();
        hashed.extend(chunk_hashed);
        done += chunk.len();
        on_progress(done.min(total), total);
    }

    let mut groups: HashMap<(u64, String), Vec<PathBuf>> = HashMap::new();
    for (size, digest, path) in hashed {
        groups.entry((size, digest)).or_default().push(path);
    }

    let mut result: Vec<DuplicateGroup> = groups
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|((size, sha256), mut paths)| {
            paths.sort();
            DuplicateGroup { sha256, size, paths }
        })
        .collect();

    // Biggest wasted-space groups first — most useful thing to show a user first.
    result.sort_by(|a, b| {
        let waste_a = a.size * (a.paths.len() as u64 - 1);
        let waste_b = b.size * (b.paths.len() as u64 - 1);
        waste_b.cmp(&waste_a)
    });

    Ok(result)
}

pub fn total_wasted_bytes(groups: &[DuplicateGroup]) -> u64 {
    groups.iter().map(|g| g.size * (g.paths.len() as u64 - 1)).sum()
}

/// Deletes every path in `to_delete` via the OS trash (never a hard delete —
/// this is the "confirmation + rollback" safety net from the spec). The
/// caller (UI layer) is responsible for having already shown a confirmation
/// dialog and for choosing which file in the group to *keep* (i.e. NOT
/// included in `to_delete`).
pub fn delete_duplicates(to_delete: &[PathBuf]) -> Vec<(PathBuf, crate::Result<()>)> {
    to_delete
        .iter()
        .map(|p| (p.clone(), crate::fs_utils::safe_delete(p).map_err(|e| e)))
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
    fn groups_byte_identical_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a.txt"), b"same content here");
        touch(&root.join("sub/b.txt"), b"same content here");
        touch(&root.join("c.txt"), b"different content!!");
        touch(&root.join("d.txt"), b"same size, diff!"); // same length as "same content here"? adjust below

        let groups = find_duplicates(&[root.to_path_buf()], &DuplicateScanOptions::default(), |_, _| {}).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].paths.len(), 2);
    }

    #[test]
    fn min_size_filter_excludes_small_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a.txt"), b"hi");
        touch(&root.join("b.txt"), b"hi");

        let opts = DuplicateScanOptions { min_size: 1024, ..Default::default() };
        let groups = find_duplicates(&[root.to_path_buf()], &opts, |_, _| {}).unwrap();
        assert!(groups.is_empty());
    }
}
