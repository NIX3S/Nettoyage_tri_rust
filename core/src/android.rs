//! TakePhotoOnPhone: Android device photo retrieval via ADB.
//!
//! ADB is the pragmatic choice for a first version (works with any USB-debug
//! enabled Android phone, no app install needed). The `AdbClient` struct is
//! deliberately the *only* thing in this module that knows about ADB's CLI
//! shape — swapping in the Android MTP API or a companion app later means
//! implementing a new struct with the same public methods and wiring it into
//! the Tauri command instead of `AdbClient`. That's the "point d'extension"
//! called out in the spec.

use crate::fs_utils::{io_err, safe_delete, silent_command, unique_destination};
use crate::hash::sha256_file;
use crate::TransferMode;
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AndroidDevice {
    pub serial: String,
    pub model: Option<String>,
    /// "device" (ready to use), "unauthorized" (RSA key prompt not yet
    /// accepted on the phone screen), "offline", etc. — surfaced so the UI
    /// can explain *why* a connected phone doesn't show up as usable,
    /// instead of just showing nothing.
    pub state: String,
}

/// Which files get *deleted from the phone* after a successful pull, when
/// `mode` is `Cut`. `mode: Copy` always ignores this — the rule only ever
/// narrows what "Cut" deletes, it never turns a Copy into a delete.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CutRule {
    /// Cut everything that was pulled.
    All,
    /// Cut only files whose on-device modification date is on/after this date.
    OnOrAfterDate(NaiveDate),
    /// Cut only files whose on-device modification date is strictly before this date.
    BeforeDate(NaiveDate),
    /// Cut only files older than N days (e.g. "plus d'un an" -> 365).
    OlderThanDays(u32),
    /// Cut only files newer than/within N days.
    NewerThanDays(u32),
}

impl Default for CutRule {
    fn default() -> Self {
        CutRule::All
    }
}

/// How the requested Android folders map onto local folders. Answers "I
/// asked for Camera and it has a pile of Camera1/Camera2/... subfolders —
/// do I want that structure kept, trimmed to just what I asked for, or
/// dumped into one single folder with no subfolders at all?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullStructure {
    /// Keep each requested folder's full internal structure (subfolders
    /// included) — the default, and the fastest: one `adb pull` per
    /// requested folder, ADB handles the recursion itself.
    FullTree,
    /// Only the files directly inside each requested folder — subfolders
    /// are not descended into at all (e.g. asking for `Camera` skips
    /// `Camera/Camera1`, `Camera/.thumbnails`, etc.).
    RequestedFoldersOnly,
    /// Every file from every requested folder, subfolders included, lands
    /// directly in one shared destination folder — no subfolders
    /// re-created locally. Closest to the original url_checker.py's
    /// flattening behavior.
    Flatten,
}

impl Default for PullStructure {
    fn default() -> Self {
        PullStructure::FullTree
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoPullOptions {
    pub device_serial: String,
    /// Android-side folders to pull from, e.g. "/storage/emulated/0/DCIM/Camera"
    /// or just "DCIM/Camera" (auto-prefixed with /storage/emulated/0/).
    pub android_paths: Vec<String>,
    pub destination: PathBuf,
    pub mode: TransferMode,
    pub cut_rule: CutRule,
    pub pull_structure: PullStructure,
    /// After pulling, remove byte-identical duplicates among the files just
    /// pulled (keeps the first copy encountered, trashes the rest). Content
    /// hashing happens locally on the pulled copies — ADB gives no cheap way
    /// to hash on-device before transfer.
    pub dedupe_by_hash: bool,
}

pub struct AdbClient {
    adb_path: PathBuf,
}

/// Wraps a single-quoted shell argument, escaping embedded quotes, so paths
/// with spaces/accents survive being sent through `adb shell "<command>"` as
/// one string (Rust's `Command` never invokes a local shell, but the string
/// we hand to `adb shell` is itself interpreted by the *device's* shell).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl AdbClient {
    /// `adb_path` is the full path to adb.exe / adb; pass `PathBuf::from("adb")`
    /// to rely on PATH.
    pub fn new(adb_path: impl Into<PathBuf>) -> Self {
        Self { adb_path: adb_path.into() }
    }

    fn run(&self, args: &[&str]) -> crate::Result<String> {
        let mut cmd = silent_command(&self.adb_path.to_string_lossy());
        // Same reasoning as the Optimizer's run_external: force adb's own
        // folder as working directory so it reliably finds its companion
        // DLLs (AdbWinApi.dll, AdbWinUsbApi.dll) next to it. Skip this when
        // adb_path is just a bare name relying on PATH (its "parent" would
        // be an empty path, which isn't a valid working directory).
        if let Some(dir) = self.adb_path.parent() {
            if !dir.as_os_str().is_empty() {
                cmd.current_dir(dir);
            }
        }
        let output = cmd
            .args(args)
            .output()
            .map_err(|e| crate::FtpError::Adb(format!("failed to run adb: {e}")))?;
        if !output.status.success() {
            return Err(crate::FtpError::Adb(String::from_utf8_lossy(&output.stderr).to_string()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Runs a raw command string through the device shell (as opposed to
    /// `run`, which passes each arg through untouched to `adb` itself).
    fn run_shell(&self, serial: &str, remote_command: &str) -> crate::Result<String> {
        self.run(&["-s", serial, "shell", remote_command])
    }

    pub fn check_available(&self) -> crate::Result<()> {
        self.run(&["version"]).map(|_| ())
    }

    pub fn list_devices(&self) -> crate::Result<Vec<AndroidDevice>> {
        let out = self.run(&["devices", "-l"])?;
        let devices = out
            .lines()
            .skip(1) // header line: "List of devices attached"
            .filter(|l| !l.trim().is_empty())
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let serial = parts.next()?.to_string();
                let state = parts.next().unwrap_or("unknown").to_string();
                let model = line
                    .split_whitespace()
                    .find(|tok| tok.starts_with("model:"))
                    .map(|tok| tok.trim_start_matches("model:").to_string());
                Some(AndroidDevice { serial, model, state })
            })
            .collect();
        Ok(devices)
    }

    /// Pulls one Android-side folder to a local destination, preserving its
    /// internal structure. Used for `PullStructure::FullTree`.
    pub fn pull(&self, serial: &str, android_path: &str, local_dest: &Path) -> crate::Result<()> {
        std::fs::create_dir_all(local_dest).map_err(io_err(local_dest))?;
        self.run(&["-s", serial, "pull", android_path, &local_dest.to_string_lossy()])?;
        Ok(())
    }

    /// Pulls a single remote file into a local folder (creating it if
    /// needed), returning the final local path. Used for
    /// `PullStructure::RequestedFoldersOnly`/`Flatten`, where we enumerate
    /// files ourselves rather than letting `adb pull` recurse.
    pub fn pull_file(&self, serial: &str, remote_file: &str, local_dir: &Path) -> crate::Result<PathBuf> {
        std::fs::create_dir_all(local_dir).map_err(io_err(local_dir))?;
        let file_name = remote_file.rsplit('/').next().unwrap_or("file");
        let local_path = unique_destination(local_dir.join(file_name));
        self.run(&["-s", serial, "pull", remote_file, &local_path.to_string_lossy()])?;
        Ok(local_path)
    }

    /// Deletes an entire folder's contents on the device — the fast path
    /// used when `CutRule::All` applies with `PullStructure::FullTree` (no
    /// need to enumerate files first).
    pub fn remove_folder(&self, serial: &str, android_path: &str) -> crate::Result<()> {
        let cmd = format!("rm -rf {}", shell_quote(android_path));
        self.run_shell(serial, &cmd)?;
        Ok(())
    }

    /// Lists every file path under `android_path`. `recursive = false`
    /// restricts to files directly inside that folder (`find -maxdepth 1`,
    /// matching `PullStructure::RequestedFoldersOnly`); `recursive = true`
    /// descends into subfolders too (matching `FullTree`/`Flatten`).
    pub fn list_files(&self, serial: &str, android_path: &str, recursive: bool) -> crate::Result<Vec<String>> {
        let depth = if recursive { String::new() } else { "-maxdepth 1 ".to_string() };
        let cmd = format!("find {} {depth}-type f", shell_quote(android_path));
        let out = self.run_shell(serial, &cmd)?;
        Ok(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
    }

    /// Same as `list_files`, but also fetches each file's modification time
    /// — needed for date-based cut rules. One combined shell call (`find
    /// ... -exec stat`) rather than one round-trip per file.
    pub fn list_files_with_mtime(&self, serial: &str, android_path: &str, recursive: bool) -> crate::Result<Vec<(i64, String)>> {
        let depth = if recursive { String::new() } else { "-maxdepth 1 ".to_string() };
        let cmd = format!(
            "find {} {depth}-type f -exec stat -c '%Y|%n' {{}} \\;",
            shell_quote(android_path)
        );
        let out = self.run_shell(serial, &cmd)?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let (epoch, path) = line.split_once('|')?;
                Some((epoch.trim().parse::<i64>().ok()?, path.trim().to_string()))
            })
            .collect())
    }

    /// Deletes a specific list of files on the device. Individual `rm` calls
    /// (rather than one giant command) so a single bad path can't abort the
    /// whole batch, and so per-file progress can be reported.
    pub fn remove_files(
        &self,
        serial: &str,
        android_paths: &[String],
        mut on_each: impl FnMut(usize, usize, &str),
    ) -> Vec<(String, crate::Result<()>)> {
        let total = android_paths.len();
        android_paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let cmd = format!("rm -f {}", shell_quote(path));
                let result = self.run_shell(serial, &cmd).map(|_| ());
                on_each(i + 1, total, path);
                (path.clone(), result)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PhotoPullReport {
    pub folders_pulled: Vec<String>,
    pub files_cut_on_device: usize,
    pub duplicates_removed_locally: usize,
    pub errors: Vec<String>,
}

fn cutoff_epoch_for_days(days: u32) -> i64 {
    Utc::now().timestamp() - (days as i64) * 86_400
}

fn matches_cut_rule(epoch: i64, rule: &CutRule) -> bool {
    match rule {
        CutRule::All => true,
        CutRule::OnOrAfterDate(d) => epoch >= d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
        CutRule::BeforeDate(d) => epoch < d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
        CutRule::OlderThanDays(n) => epoch < cutoff_epoch_for_days(*n),
        CutRule::NewerThanDays(n) => epoch >= cutoff_epoch_for_days(*n),
    }
}

/// Removes byte-identical duplicates among the files just pulled into
/// `local_dest` (recursively), keeping the first copy of each and trashing
/// the rest. Returns how many were removed.
fn dedupe_local_folder(local_dest: &Path) -> crate::Result<usize> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut removed = 0usize;
    for entry in WalkDir::new(local_dest).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if let Ok(hash) = sha256_file(path) {
            if seen.contains(&hash) {
                if safe_delete(path).is_ok() {
                    removed += 1;
                }
            } else {
                seen.insert(hash);
            }
        }
    }
    Ok(removed)
}

/// Android folder paths in the UI/config file are commonly given relative
/// to the shared storage root for convenience (`DCIM/Camera`, `Download`,
/// matching what `folders.txt` used in the original url_checker.py, which
/// concatenated a `base_android_path`). `adb pull`/`shell` need the full
/// absolute path, so anything not already starting with `/` gets this
/// prefix added automatically. An already-absolute path (`/sdcard/...`,
/// `/storage/...`) is left untouched.
fn normalize_android_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/storage/emulated/0/{path}")
    }
}

/// High-level orchestration used by the TakePhotoOnPhone tab: pull every
/// requested folder (in whichever structure was asked for), optionally
/// dedupe the local copies, then apply the cut rule.
pub fn run_photo_pull(
    adb: &AdbClient,
    opts: &PhotoPullOptions,
    mut on_progress: impl FnMut(usize, usize, &str),
) -> crate::Result<PhotoPullReport> {
    match opts.pull_structure {
        PullStructure::FullTree => run_pull_full_tree(adb, opts, &mut on_progress),
        PullStructure::RequestedFoldersOnly => run_pull_selective(adb, opts, &mut on_progress, false),
        PullStructure::Flatten => run_pull_selective(adb, opts, &mut on_progress, true),
    }
}

/// `FullTree`: one `adb pull` per requested folder, letting ADB handle the
/// recursion — fastest option, and the original behavior before
/// `PullStructure` existed.
fn run_pull_full_tree(
    adb: &AdbClient,
    opts: &PhotoPullOptions,
    on_progress: &mut impl FnMut(usize, usize, &str),
) -> crate::Result<PhotoPullReport> {
    let mut report = PhotoPullReport::default();
    let total = opts.android_paths.len();

    for (i, raw_path) in opts.android_paths.iter().enumerate() {
        let android_path = &normalize_android_path(raw_path);
        on_progress(i, total, android_path);
        let folder_name = android_path.rsplit('/').next().unwrap_or("export");
        let local_dest = unique_destination(opts.destination.join(folder_name));

        match adb.pull(&opts.device_serial, android_path, &local_dest) {
            Ok(()) => {
                report.folders_pulled.push(android_path.clone());

                if opts.dedupe_by_hash {
                    match dedupe_local_folder(&local_dest) {
                        Ok(n) => report.duplicates_removed_locally += n,
                        Err(e) => report.errors.push(format!("dedup {android_path}: {e}")),
                    }
                }

                if opts.mode == TransferMode::Cut {
                    match &opts.cut_rule {
                        CutRule::All => {
                            if let Err(e) = adb.remove_folder(&opts.device_serial, android_path) {
                                report.errors.push(format!("cut failed for {android_path}: {e}"));
                            } else {
                                // No exact file count for a whole-folder rm;
                                // approximate with what we actually pulled locally.
                                report.files_cut_on_device += WalkDir::new(&local_dest)
                                    .into_iter()
                                    .filter_map(|e| e.ok())
                                    .filter(|e| e.file_type().is_file())
                                    .count();
                            }
                        }
                        rule => match adb.list_files_with_mtime(&opts.device_serial, android_path, true) {
                            Ok(files) => {
                                let matched: Vec<String> = files
                                    .into_iter()
                                    .filter(|(epoch, _)| matches_cut_rule(*epoch, rule))
                                    .map(|(_, path)| path)
                                    .collect();
                                let results = adb.remove_files(&opts.device_serial, &matched, |done, tot, path| {
                                    on_progress(done, tot, path)
                                });
                                for (path, res) in results {
                                    match res {
                                        Ok(()) => report.files_cut_on_device += 1,
                                        Err(e) => report.errors.push(format!("cut {path}: {e}")),
                                    }
                                }
                            }
                            Err(e) => report.errors.push(format!("listing {android_path} for cut: {e}")),
                        },
                    }
                }
            }
            Err(e) => report.errors.push(format!("{android_path}: {e}")),
        }
    }

    on_progress(total, total, "");
    Ok(report)
}

/// `RequestedFoldersOnly` (`flatten = false`) / `Flatten` (`flatten =
/// true`): we enumerate files ourselves (so we know exactly which ones were
/// pulled — cutting means removing exactly those, never more), then pull
/// each individually. `flatten` controls both how deep we look
/// (`RequestedFoldersOnly` only looks at each folder's direct children) and
/// where files land locally (one shared folder for `Flatten`, one
/// per-requested-folder otherwise).
fn run_pull_selective(
    adb: &AdbClient,
    opts: &PhotoPullOptions,
    on_progress: &mut impl FnMut(usize, usize, &str),
    flatten: bool,
) -> crate::Result<PhotoPullReport> {
    let mut report = PhotoPullReport::default();
    let normalized_paths: Vec<String> = opts.android_paths.iter().map(|p| normalize_android_path(p)).collect();
    report.folders_pulled = normalized_paths.clone();

    // Phase 1: enumerate every remote file across all requested folders.
    // `RequestedFoldersOnly` looks one level deep only; `Flatten` recurses.
    let mut all_files: Vec<(String, String)> = Vec::new(); // (source_folder, remote_file_path)
    for android_path in &normalized_paths {
        match adb.list_files(&opts.device_serial, android_path, flatten) {
            Ok(files) => all_files.extend(files.into_iter().map(|f| (android_path.clone(), f))),
            Err(e) => report.errors.push(format!("{android_path}: {e}")),
        }
    }

    // Phase 2: pull each file to its destination.
    let total = all_files.len();
    let mut pulled: HashSet<String> = HashSet::new();
    let mut per_folder_dest: HashMap<String, PathBuf> = HashMap::new();

    for (i, (source_folder, remote_file)) in all_files.iter().enumerate() {
        on_progress(i, total, remote_file);
        let local_dir = if flatten {
            opts.destination.clone()
        } else {
            per_folder_dest
                .entry(source_folder.clone())
                .or_insert_with(|| {
                    let folder_name = source_folder.rsplit('/').next().unwrap_or("export");
                    unique_destination(opts.destination.join(folder_name))
                })
                .clone()
        };
        match adb.pull_file(&opts.device_serial, remote_file, &local_dir) {
            Ok(_) => {
                pulled.insert(remote_file.clone());
            }
            Err(e) => report.errors.push(format!("{remote_file}: {e}")),
        }
    }
    on_progress(total, total, "");

    // Phase 3: dedupe locally, if asked.
    if opts.dedupe_by_hash {
        let dirs_to_check: Vec<PathBuf> =
            if flatten { vec![opts.destination.clone()] } else { per_folder_dest.values().cloned().collect() };
        for dir in dirs_to_check {
            match dedupe_local_folder(&dir) {
                Ok(n) => report.duplicates_removed_locally += n,
                Err(e) => report.errors.push(format!("dedup {}: {e}", dir.display())),
            }
        }
    }

    // Phase 4: cut, if asked — exactly the files we actually pulled (never
    // more, unlike a blanket `rm -rf` would), optionally narrowed further
    // by the date-based rule.
    if opts.mode == TransferMode::Cut && !pulled.is_empty() {
        let candidates: Vec<String> = match &opts.cut_rule {
            CutRule::All => pulled.into_iter().collect(),
            rule => {
                let mut matched = Vec::new();
                for android_path in &normalized_paths {
                    if let Ok(files) = adb.list_files_with_mtime(&opts.device_serial, android_path, flatten) {
                        for (epoch, path) in files {
                            if pulled.contains(&path) && matches_cut_rule(epoch, rule) {
                                matched.push(path);
                            }
                        }
                    }
                }
                matched
            }
        };
        let results = adb.remove_files(&opts.device_serial, &candidates, |done, tot, path| on_progress(done, tot, path));
        for (path, res) in results {
            match res {
                Ok(()) => report.files_cut_on_device += 1,
                Err(e) => report.errors.push(format!("cut {path}: {e}")),
            }
        }
    }

    Ok(report)
}
