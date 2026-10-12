use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FtpError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("path does not exist or is not a directory: {0}")]
    InvalidDir(PathBuf),
    #[error("external tool not found: {0} (install it and make sure it's on PATH)")]
    MissingTool(String),
    #[error("adb error: {0}")]
    Adb(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> FtpError {
    let path = path.into();
    move |source| FtpError::Io { path, source }
}

/// Reveal a file/folder in the OS's native file manager (Explorer / Finder / Nautilus...).
/// If given a file, we open its *parent directory* — that's what "open location" means
/// to a normal user, not opening the file itself.
pub fn reveal_in_file_manager(path: &Path) -> crate::Result<()> {
    let target = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    open::that(target).map_err(io_err(target))?;
    Ok(())
}

/// Safe delete: sends to the OS Recycle Bin / Trash instead of unlinking,
/// so "Find Duplicates" deletions are always recoverable by the user.
/// This is the safety net requested for the Optimizer/Duplicates modules
/// ("rollback if possible").
pub fn safe_delete(path: &Path) -> crate::Result<()> {
    trash::delete(path).map_err(|e| FtpError::Other(format!("trash delete failed for {}: {e}", path.display())))
}

/// Ensures a destination path doesn't clobber an existing file by appending
/// `_1`, `_2`, ... before the extension. Mirrors the collision handling used
/// throughout the original Python scripts (tri.py, find_duplicates.py, etc).
pub fn unique_destination(mut candidate: PathBuf) -> PathBuf {
    if !candidate.exists() {
        return candidate;
    }
    let stem = candidate
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = candidate
        .extension()
        .map(|s| s.to_string_lossy().to_string());
    let parent = candidate
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    let mut counter = 1u32;
    loop {
        let name = match &ext {
            Some(e) if !e.is_empty() => format!("{stem}_{counter}.{e}"),
            _ => format!("{stem}_{counter}"),
        };
        candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

/// Copy or move a single file, creating parent directories as needed and
/// avoiding collisions via `unique_destination`. Returns the final path used.
pub fn transfer_file(
    src: &Path,
    dest: &Path,
    mode: crate::TransferMode,
) -> crate::Result<PathBuf> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let final_dest = unique_destination(dest.to_path_buf());
    match mode {
        crate::TransferMode::Copy => {
            std::fs::copy(src, &final_dest).map_err(io_err(src))?;
        }
        crate::TransferMode::Cut => {
            // Cross-device moves can fail with `std::fs::rename`; fall back to copy+delete.
            if std::fs::rename(src, &final_dest).is_err() {
                std::fs::copy(src, &final_dest).map_err(io_err(src))?;
                std::fs::remove_file(src).map_err(io_err(src))?;
            }
        }
    }
    Ok(final_dest)
}

pub fn human_size(bytes: u64) -> String {
    let units = ["o", "Ko", "Mo", "Go", "To"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < units.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{bytes} {}", units[0])
    } else {
        format!("{size:.1} {}", units[unit_idx])
    }
}

/// Builds a `Command` that never flashes a console window on Windows.
///
/// Every external tool call in this app (ffmpeg, qpdf, gs, adb...) goes
/// through this instead of `Command::new` directly. Without the
/// `CREATE_NO_WINDOW` flag, each call briefly pops a black terminal window —
/// harmless technically, but it looks like the app is crashing to a
/// non-technical user, which is exactly who this app is for.
pub fn silent_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Resolves an external tool (ffmpeg, qpdf, gs...) to an actual path to run,
/// checking in order:
///   1. a `tools/<name>(.exe)` folder sitting next to the running
///      executable — this is where `scripts/vendor-tools.*` downloads
///      portable builds so the app "just works" without the user installing
///      anything separately (see README "Fournir qpdf/ffmpeg avec l'app"),
///   2. the system PATH.
/// Returns `None` if neither has it, so the caller can report a clear
/// `MissingTool` error instead of a cryptic "exited with 127".
pub fn resolve_tool(name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let tools = dir.join("tools");
            // Different vendoring scripts lay things out differently (a
            // flat file, or the tool's own release folder copied wholesale
            // so its DLLs travel with it — e.g. qpdf's "bin/" subfolder).
            // Try the common shapes before giving up on the bundled copy.
            let candidates = [
                tools.join(&exe_name),                    // tools/qpdf.exe
                tools.join(name).join("bin").join(&exe_name), // tools/qpdf/bin/qpdf.exe
                tools.join(name).join(&exe_name),          // tools/qpdf/qpdf.exe
            ];
            if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
                return Some(found);
            }
        }
    }

    which::which(name).ok()
}
