//! Crash recovery: remember where a long operation was, so that after a
//! crash (or a power cut, or a killed window) it can resume instead of
//! starting over.
//!
//! Two complementary mechanisms, both strictly best-effort — a failure to
//! write recovery data must NEVER break the actual operation:
//!
//!   * [`RunSession`] — a per-operation journal. At start a small `<tab>.run.json`
//!     "marker" stores the operation's parameters; every finished file is
//!     appended to `<tab>.done.jsonl` (flushed immediately). On normal
//!     completion both files are deleted, so their mere presence at the next
//!     launch means "this run was interrupted". Resuming replays the saved
//!     parameters and skips every file already in the journal.
//!   * [`HashCache`] — Find Duplicate's expensive step is hashing, so hashes
//!     already computed are kept (keyed by path + size + modification time)
//!     and reused by later scans. After a crash the re-run is nearly instant
//!     up to where it stopped; and since a stale entry is detected via
//!     size/mtime, it also speeds up ordinary repeated scans.

use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// One finished file. Optional fields carry just enough to rebuild the final
/// report (and, for TriPhotosVideo, the dedup hash set) after a resume.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Source path.
    pub p: String,
    /// Destination path, when the operation produced one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<String>,
    /// SHA-256 (TriPhotosVideo dedup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<String>,
    /// Skipped as a duplicate.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dup: bool,
    /// Filed under "NSP" (no usable date).
    #[serde(default, skip_serializing_if = "is_false")]
    pub u: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl JournalEntry {
    pub fn for_path(path: &Path) -> Self {
        JournalEntry { p: path.to_string_lossy().to_string(), ..Default::default() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Marker {
    tab: String,
    started_at: String,
    params: serde_json::Value,
}

/// An operation that was started but never finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnfinishedRun {
    pub tab: String,
    pub started_at: String,
    pub done_count: usize,
    pub params: serde_json::Value,
}

fn marker_path(dir: &Path, tab: &str) -> PathBuf {
    dir.join(format!("{tab}.run.json"))
}
fn journal_path(dir: &Path, tab: &str) -> PathBuf {
    dir.join(format!("{tab}.done.jsonl"))
}

fn read_journal(path: &Path) -> HashMap<String, JournalEntry> {
    let mut map = HashMap::new();
    if let Ok(file) = File::open(path) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            // A crash can leave a half-written last line: just skip it.
            if let Ok(entry) = serde_json::from_str::<JournalEntry>(&line) {
                map.insert(entry.p.clone(), entry);
            }
        }
    }
    map
}

pub struct RunSession {
    dir: PathBuf,
    tab: String,
    writer: Option<BufWriter<File>>,
    /// Files finished before the crash (empty for a fresh run).
    pub done: HashMap<String, JournalEntry>,
    /// `true` when this session continues an interrupted run.
    pub resumed: bool,
}

impl RunSession {
    /// Starts journaling. With `resume = true` and an existing marker for this
    /// tab, the previously finished files are loaded; otherwise any stale
    /// journal is discarded and a new marker is written. Returns `None` if
    /// the recovery folder is unusable — callers just run without recovery.
    pub fn begin(dir: &Path, tab: &str, params: &serde_json::Value, resume: bool) -> Option<Self> {
        std::fs::create_dir_all(dir).ok()?;
        let marker = marker_path(dir, tab);
        let journal = journal_path(dir, tab);

        let resuming = resume && marker.is_file();
        let done = if resuming { read_journal(&journal) } else { HashMap::new() };

        if !resuming {
            let m = Marker {
                tab: tab.to_string(),
                started_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                params: params.clone(),
            };
            let tmp = dir.join(format!("{tab}.run.json.tmp"));
            std::fs::write(&tmp, serde_json::to_vec(&m).ok()?).ok()?;
            std::fs::rename(&tmp, &marker).ok()?;
            let _ = std::fs::remove_file(&journal);
        }

        let file = OpenOptions::new().create(true).append(true).open(&journal).ok()?;
        Some(RunSession {
            dir: dir.to_path_buf(),
            tab: tab.to_string(),
            writer: Some(BufWriter::new(file)),
            done,
            resumed: resuming,
        })
    }

    pub fn is_done(&self, path: &Path) -> bool {
        self.done.contains_key(path.to_string_lossy().as_ref())
    }

    pub fn entry(&self, path: &Path) -> Option<&JournalEntry> {
        self.done.get(path.to_string_lossy().as_ref())
    }

    /// Records a finished file. Flushed right away so that a crash right after
    /// loses at most the file being processed at that moment.
    pub fn mark(&mut self, entry: JournalEntry) {
        if let Some(w) = self.writer.as_mut() {
            if let Ok(line) = serde_json::to_string(&entry) {
                let ok = writeln!(w, "{line}").is_ok() && w.flush().is_ok();
                if !ok {
                    self.writer = None; // disk full / folder gone: stop journaling, keep working
                }
            }
        }
        self.done.insert(entry.p.clone(), entry);
    }

    /// Normal completion: nothing to recover anymore.
    pub fn finish(mut self) {
        self.writer = None;
        let _ = std::fs::remove_file(marker_path(&self.dir, &self.tab));
        let _ = std::fs::remove_file(journal_path(&self.dir, &self.tab));
    }
}

/// Operations whose marker is still on disk, i.e. that never finished.
pub fn list_unfinished(dir: &Path) -> Vec<UnfinishedRun> {
    let mut runs = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return runs };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !name.ends_with(".run.json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(marker) = serde_json::from_slice::<Marker>(&bytes) else { continue };
        let done_count = read_journal(&journal_path(dir, &marker.tab)).len();
        runs.push(UnfinishedRun {
            tab: marker.tab,
            started_at: marker.started_at,
            done_count,
            params: marker.params,
        });
    }
    runs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    runs
}

/// Forgets an interrupted run (the user chose "Ignorer").
pub fn discard(dir: &Path, tab: &str) {
    let _ = std::fs::remove_file(marker_path(dir, tab));
    let _ = std::fs::remove_file(journal_path(dir, tab));
}

// ---------------------------------------------------------------------
// Hash cache
// ---------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct CacheLine {
    p: String,
    s: u64,
    m: i64,
    h: String,
}

pub struct HashCache {
    path: PathBuf,
    map: HashMap<String, (u64, i64, String)>,
    writer: Option<BufWriter<File>>,
}

/// Modification time in milliseconds since the epoch (0 if unavailable —
/// then entries simply never match a changed file's real mtime).
pub fn mtime_millis(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl HashCache {
    pub fn open(dir: &Path) -> Self {
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("hashcache.jsonl");
        let mut map: HashMap<String, (u64, i64, String)> = HashMap::new();
        let mut lines = 0usize;
        if let Ok(file) = File::open(&path) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                lines += 1;
                if let Ok(c) = serde_json::from_str::<CacheLine>(&line) {
                    map.insert(c.p, (c.s, c.m, c.h));
                }
            }
        }
        // Compact when the append-only file carries lots of superseded lines.
        if lines > map.len() * 2 + 1000 {
            let tmp = dir.join("hashcache.jsonl.tmp");
            if let Ok(f) = File::create(&tmp) {
                let mut w = BufWriter::new(f);
                let ok = map.iter().all(|(p, (s, m, h))| {
                    let line = CacheLine { p: p.clone(), s: *s, m: *m, h: h.clone() };
                    serde_json::to_string(&line).is_ok_and(|l| writeln!(w, "{l}").is_ok())
                }) && w.flush().is_ok();
                drop(w);
                if ok {
                    let _ = std::fs::rename(&tmp, &path);
                } else {
                    let _ = std::fs::remove_file(&tmp);
                }
            }
        }
        let writer = OpenOptions::new().create(true).append(true).open(&path).ok().map(BufWriter::new);
        HashCache { path, map, writer }
    }

    /// Cached SHA-256, only if the file still has the same size and mtime.
    pub fn get(&self, path: &Path, size: u64, mtime: i64) -> Option<&str> {
        let (s, m, h) = self.map.get(path.to_string_lossy().as_ref())?;
        (*s == size && *m == mtime).then_some(h.as_str())
    }

    pub fn insert(&mut self, path: &Path, size: u64, mtime: i64, hash: &str) {
        let key = path.to_string_lossy().to_string();
        if let Some(w) = self.writer.as_mut() {
            let line = CacheLine { p: key.clone(), s: size, m: mtime, h: hash.to_string() };
            if serde_json::to_string(&line).map(|l| writeln!(w, "{l}")).is_err() {
                self.writer = None;
            }
        }
        self.map.insert(key, (size, mtime, hash.to_string()));
    }

    pub fn flush(&mut self) {
        if let Some(w) = self.writer.as_mut() {
            if w.flush().is_err() {
                self.writer = None;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_run_is_listed_and_resumable_then_cleaned_on_finish() {
        let dir = tempfile::tempdir().unwrap();
        let params = serde_json::json!({"source": "D:/x"});

        let mut s = RunSession::begin(dir.path(), "optimizer", &params, false).unwrap();
        s.mark(JournalEntry::for_path(Path::new("a.pdf")));
        s.mark(JournalEntry::for_path(Path::new("b.pdf")));
        drop(s); // simulated crash: no finish()

        let runs = list_unfinished(dir.path());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].tab, "optimizer");
        assert_eq!(runs[0].done_count, 2);
        assert_eq!(runs[0].params, params);

        let s = RunSession::begin(dir.path(), "optimizer", &params, true).unwrap();
        assert!(s.resumed);
        assert!(s.is_done(Path::new("a.pdf")));
        assert!(!s.is_done(Path::new("c.pdf")));
        s.finish();
        assert!(list_unfinished(dir.path()).is_empty());
    }

    #[test]
    fn fresh_start_discards_old_journal() {
        let dir = tempfile::tempdir().unwrap();
        let params = serde_json::json!({});
        let mut s = RunSession::begin(dir.path(), "getall", &params, false).unwrap();
        s.mark(JournalEntry::for_path(Path::new("a")));
        drop(s);
        let s = RunSession::begin(dir.path(), "getall", &params, false).unwrap();
        assert!(!s.resumed);
        assert!(s.done.is_empty());
    }

    #[test]
    fn truncated_last_line_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let params = serde_json::json!({});
        let mut s = RunSession::begin(dir.path(), "x", &params, false).unwrap();
        s.mark(JournalEntry::for_path(Path::new("ok")));
        drop(s);
        let mut f = OpenOptions::new().append(true).open(journal_path(dir.path(), "x")).unwrap();
        write!(f, "{{\"p\":\"half").unwrap();
        let s = RunSession::begin(dir.path(), "x", &params, true).unwrap();
        assert_eq!(s.done.len(), 1);
    }

    #[test]
    fn hash_cache_roundtrip_and_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = HashCache::open(dir.path());
        c.insert(Path::new("f"), 10, 111, "abc");
        c.flush();
        drop(c);
        let c = HashCache::open(dir.path());
        assert_eq!(c.get(Path::new("f"), 10, 111), Some("abc"));
        assert_eq!(c.get(Path::new("f"), 11, 111), None); // size changed
        assert_eq!(c.get(Path::new("f"), 10, 112), None); // mtime changed
    }
}
