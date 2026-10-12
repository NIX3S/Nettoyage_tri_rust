//! Every `#[tauri::command]` here does three things and nothing more:
//!   1. deserialize args from the frontend,
//!   2. call into `ftp-core` **off the main/UI thread** (via
//!      `tauri::async_runtime::spawn_blocking`), emitting a `(done, total)`
//!      progress event while it runs so the UI can show a real percentage
//!      and the current file without freezing,
//!   3. serialize the result (or a stringified error) back to JS.
//! All actual logic lives in `ftp-core` — see that crate for the parts that
//! matter and are unit-tested.
//!
//! Why `spawn_blocking` specifically: a plain (non-`async`) Tauri command
//! runs on the same thread as the webview's own event loop, so any
//! synchronous, CPU/IO-heavy call (walking thousands of files, hashing,
//! shelling out to ffmpeg...) blocks window repaints and input entirely
//! until it returns — exactly the "GUI freezes, everything appears at the
//! end" symptom. Marking the command `async` and running the real work
//! inside `spawn_blocking` moves it onto a dedicated worker thread, so the
//! window keeps repainting and the progress events keep arriving live.

use ftp_core::android::{self, AdbClient, PhotoPullOptions, PhotoPullReport};
use ftp_core::comparison::{self, AutoUpdateConfig, ComparisonResult, FileEntry};
use ftp_core::duplicates::{self, DuplicateGroup, DuplicateScanOptions};
use ftp_core::getall::{self, GetAllOptions, GetAllReport};
use ftp_core::optimizer::{self, OptimizeOptions, OptimizeReport};
use ftp_core::sort_media::{self, SortMediaOptions, SortMediaReport};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, State, WebviewWindow};

pub struct AppState {
    pub adb_path: String,
}

impl Default for AppState {
    fn default() -> Self {
        // Same lookup order as the Optimizer's ffmpeg/qpdf: a bundled
        // `tools/adb(.exe)` next to the executable (see
        // `scripts/vendor-tools.ps1`) takes priority, falling back to
        // whatever `adb` resolves to on the system PATH. The Advanced menu
        // in the TakePhotoOnPhone tab still lets the user override this
        // with an explicit path (mirrors CONFIG["adb_path"] in the
        // original url_checker.py).
        let adb_path = ftp_core::fs_utils::resolve_tool("adb")
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "adb".to_string());
        Self { adb_path }
    }
}

#[derive(Serialize, Clone)]
pub struct ProgressEvent<'a> {
    pub tab: &'a str,
    pub done: usize,
    /// `0` means "still counting / indeterminate" — the frontend shows a
    /// pulsing bar instead of a percentage in that case.
    pub total: usize,
    pub current: Option<String>,
}

fn emit_progress(window: &WebviewWindow, tab: &str, done: usize, total: usize, current: Option<String>) {
    let _ = window.emit("ftp://progress", ProgressEvent { tab, done, total, current });
}

/// Caps how often progress events actually go out. Without this, a batch of
/// several thousand small/fast files (e.g. Optimizer over "Tout" on a whole
/// drive, or GetALL) can fire thousands of IPC events per second — the
/// webview can't paint that fast, so the bar visually looks frozen and then
/// "catches up" all at once at the end, which is exactly the symptom this
/// throttle fixes. The very last event (`done == total`) always goes
/// through un-throttled so the UI reliably reaches 100%.
struct ProgressThrottle {
    last: std::time::Instant,
    interval: std::time::Duration,
}

impl ProgressThrottle {
    fn new() -> Self {
        Self {
            last: std::time::Instant::now() - std::time::Duration::from_secs(1),
            interval: std::time::Duration::from_millis(120),
        }
    }

    /// `true` if this event should actually be emitted.
    fn allow(&mut self, done: usize, total: usize) -> bool {
        if total != 0 && done >= total {
            return true; // always let the final/completion event through
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.last) >= self.interval {
            self.last = now;
            true
        } else {
            false
        }
    }
}

fn to_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Runs `f` on Tauri's blocking-task thread pool instead of the calling
/// (UI) thread, and turns a thread-join failure into the same `String`
/// error type every command already returns.
async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(to_err)
}

// ---------------------------------------------------------------------
// Shared: native folder picker (used by every "Browse" button in the UI)
// ---------------------------------------------------------------------

#[tauri::command]
pub fn pick_folder_cmd() -> Option<String> {
    rfd::FileDialog::new().pick_folder().map(|p| p.display().to_string())
}

#[tauri::command]
pub fn pick_text_file_cmd() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("Texte", &["txt"])
        .pick_file()
        .map(|p| p.display().to_string())
}

#[tauri::command]
pub fn pick_save_text_file_cmd(default_name: String) -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("Texte", &["txt"])
        .set_file_name(&default_name)
        .save_file()
        .map(|p| p.display().to_string())
}

#[tauri::command]
pub fn read_text_file_cmd(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(to_err)
}

#[tauri::command]
pub fn write_text_file_cmd(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, content).map_err(to_err)
}

// ---------------------------------------------------------------------
// Comparison tab
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn compare_folders_cmd(folder_a: String, folder_b: String) -> Result<ComparisonResult, String> {
    run_blocking(move || {
        comparison::compare_folders(&PathBuf::from(folder_a), &PathBuf::from(folder_b)).map_err(to_err)
    })
    .await?
}

#[tauri::command]
pub async fn auto_update_cmd(
    window: WebviewWindow,
    entries: Vec<FileEntry>,
    dest_root: String,
    config: AutoUpdateConfig,
) -> Result<Vec<(String, Result<String, String>)>, String> {
    run_blocking(move || {
        let mut throttle = ProgressThrottle::new();
        let results = comparison::auto_update_many(&entries, &PathBuf::from(dest_root), &config, |done, total| {
            if throttle.allow(done, total) {
                emit_progress(&window, "comparison", done, total, None);
            }
        });
        results
            .into_iter()
            .map(|(rel, res)| (rel, res.map(|p| p.display().to_string()).map_err(to_err)))
            .collect::<Vec<_>>()
    })
    .await
}

#[tauri::command]
pub fn reveal_in_file_manager_cmd(path: String) -> Result<(), String> {
    ftp_core::fs_utils::reveal_in_file_manager(&PathBuf::from(path)).map_err(to_err)
}

// ---------------------------------------------------------------------
// Find Duplicates tab
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn find_duplicates_cmd(
    window: WebviewWindow,
    roots: Vec<String>,
    options: DuplicateScanOptions,
) -> Result<Vec<DuplicateGroup>, String> {
    run_blocking(move || {
        let roots: Vec<PathBuf> = roots.into_iter().map(PathBuf::from).collect();
        let mut throttle = ProgressThrottle::new();
        duplicates::find_duplicates(&roots, &options, |done, total| {
            if throttle.allow(done, total) {
                emit_progress(&window, "duplicates", done, total, None);
            }
        })
        .map_err(to_err)
    })
    .await?
}

#[tauri::command]
pub async fn delete_duplicates_cmd(paths: Vec<String>) -> Result<Vec<(String, Result<(), String>)>, String> {
    run_blocking(move || {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        duplicates::delete_duplicates(&paths)
            .into_iter()
            .map(|(p, r)| (p.display().to_string(), r.map_err(to_err)))
            .collect::<Vec<_>>()
    })
    .await
}

// ---------------------------------------------------------------------
// Optimizer tab
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn run_optimizer_cmd(window: WebviewWindow, options: OptimizeOptions) -> Result<OptimizeReport, String> {
    run_blocking(move || {
        let mut throttle = ProgressThrottle::new();
        optimizer::run_optimizer(&options, |done, total, path| {
            if throttle.allow(done, total) {
                emit_progress(&window, "optimizer", done, total, Some(path.display().to_string()));
            }
        })
        .map_err(to_err)
    })
    .await?
}

// ---------------------------------------------------------------------
// GetALL tab
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn run_getall_cmd(window: WebviewWindow, options: GetAllOptions) -> Result<GetAllReport, String> {
    run_blocking(move || {
        let mut throttle = ProgressThrottle::new();
        getall::run_getall(&options, |done, total, path| {
            if throttle.allow(done, total) {
                emit_progress(&window, "getall", done, total, Some(path.display().to_string()));
            }
        })
        .map_err(to_err)
    })
    .await?
}

// ---------------------------------------------------------------------
// TriPhotosVideo tab
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn run_sort_media_cmd(window: WebviewWindow, options: SortMediaOptions) -> Result<SortMediaReport, String> {
    run_blocking(move || {
        let mut throttle = ProgressThrottle::new();
        sort_media::run_sort_media(&options, |done, total, path| {
            if throttle.allow(done, total) {
                emit_progress(&window, "sort_media", done, total, Some(path.display().to_string()));
            }
        })
        .map_err(to_err)
    })
    .await?
}

// ---------------------------------------------------------------------
// TakePhotoOnPhone tab
// ---------------------------------------------------------------------

#[tauri::command]
pub fn set_adb_path_cmd(state: State<'_, Mutex<AppState>>, path: String) -> Result<(), String> {
    state.lock().map_err(to_err)?.adb_path = path;
    Ok(())
}

#[tauri::command]
pub async fn list_android_devices_cmd(
    state: State<'_, Mutex<AppState>>,
) -> Result<Vec<android::AndroidDevice>, String> {
    let adb_path = state.lock().map_err(to_err)?.adb_path.clone();
    run_blocking(move || {
        let client = AdbClient::new(adb_path);
        client.check_available().map_err(to_err)?;
        client.list_devices().map_err(to_err)
    })
    .await?
}

#[tauri::command]
pub async fn run_photo_pull_cmd(
    window: WebviewWindow,
    state: State<'_, Mutex<AppState>>,
    options: PhotoPullOptions,
) -> Result<PhotoPullReport, String> {
    let adb_path = state.lock().map_err(to_err)?.adb_path.clone();
    run_blocking(move || {
        let client = AdbClient::new(adb_path);
        let mut throttle = ProgressThrottle::new();
        android::run_photo_pull(&client, &options, |done, total, current| {
            if throttle.allow(done, total) {
                emit_progress(&window, "android", done, total, Some(current.to_string()));
            }
        })
        .map_err(to_err)
    })
    .await?
}
