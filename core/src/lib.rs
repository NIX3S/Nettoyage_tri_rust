//! ftp-core: all business logic for File Toolkit Pro.
//!
//! This crate is intentionally UI-agnostic and has no dependency on Tauri.
//! `src-tauri` only translates these calls into `#[tauri::command]` wrappers
//! and forwards progress via events. This split means:
//!   - the logic is unit-testable without spinning up a webview,
//!   - the same core could power a CLI or a different GUI later.

pub mod android;
pub mod comparison;
pub mod duplicates;
pub mod fs_utils;
pub mod getall;
pub mod hash;
pub mod optimizer;
pub mod sort_media;

pub use fs_utils::FtpError;

pub type Result<T> = std::result::Result<T, FtpError>;

/// Copy or move — used by every module that relocates files, so the
/// "cut vs copy" choice is expressed the same way everywhere in the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferMode {
    Copy,
    Cut,
}
