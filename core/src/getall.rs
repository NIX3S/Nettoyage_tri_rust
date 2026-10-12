//! GetALL: gather files matching chosen categories/extensions from one or
//! more source folders into a single flat (or JSON-indexed) output folder.

use crate::fs_utils::{io_err, transfer_file};
use crate::TransferMode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Built-in categories the UI offers as checkboxes; "Extensions" below lets
/// the user narrow further or pick a single extension instead.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum FileCategory {
    Image,
    Video,
    Audio,
    Document,
}

impl FileCategory {
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            FileCategory::Image => &["jpg", "jpeg", "png", "gif", "bmp", "tiff", "webp", "heic"],
            FileCategory::Video => &["mp4", "avi", "mkv", "mov", "wmv", "flv", "webm", "m4v"],
            FileCategory::Audio => &["mp3", "wav", "flac", "aac", "ogg", "m4a"],
            FileCategory::Document => &["pdf", "docx", "doc", "xlsx", "pptx", "txt", "odt"],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetAllOptions {
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
    pub categories: Vec<FileCategory>,
    /// Extra explicit extensions (lowercase, no dot) on top of `categories`.
    /// If both `categories` and this are empty, everything is gathered.
    pub extra_extensions: Vec<String>,
    pub mode: TransferMode,
    /// Writes a `getall_index.json` in `destination` mapping every gathered
    /// file back to its original absolute path, per spec ("conserve
    /// l'ancienne position dans le PC").
    pub export_json_index: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatheredFile {
    pub original_path: PathBuf,
    pub new_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GetAllReport {
    pub gathered: Vec<GatheredFile>,
    pub errors: Vec<String>,
}

fn allowed_extensions(opts: &GetAllOptions) -> Vec<String> {
    let mut exts: Vec<String> = opts
        .categories
        .iter()
        .flat_map(|c| c.extensions().iter().map(|s| s.to_string()))
        .collect();
    exts.extend(opts.extra_extensions.iter().map(|e| e.to_lowercase()));
    exts
}

pub fn run_getall(
    opts: &GetAllOptions,
    mut on_progress: impl FnMut(usize, usize, &Path),
) -> crate::Result<GetAllReport> {
    std::fs::create_dir_all(&opts.destination).map_err(io_err(&opts.destination))?;
    let allowed = allowed_extensions(opts);
    let gather_everything = allowed.is_empty();

    let matches = |path: &Path| -> bool {
        if gather_everything {
            return true;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        allowed.contains(&ext)
    };

    // Pre-count so the UI can show a real "N / total" percentage from the
    // very first event instead of guessing.
    let mut total = 0usize;
    for source in &opts.sources {
        if !source.is_dir() {
            continue;
        }
        total += WalkDir::new(source)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && matches(e.path()))
            .count();
    }

    let mut report = GetAllReport::default();
    let mut count = 0usize;
    on_progress(0, total, &opts.destination);

    for source in &opts.sources {
        if !source.is_dir() {
            report.errors.push(format!("dossier introuvable: {}", source.display()));
            continue;
        }
        for entry in WalkDir::new(source).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !matches(path) {
                continue;
            }

            let file_name = path.file_name().unwrap_or_default();
            let dest = opts.destination.join(file_name);

            match transfer_file(path, &dest, opts.mode) {
                Ok(final_dest) => {
                    count += 1;
                    report.gathered.push(GatheredFile {
                        original_path: path.to_path_buf(),
                        new_path: final_dest,
                    });
                }
                Err(e) => report.errors.push(format!("{}: {e}", path.display())),
            }
            on_progress(count, total, path);
        }
    }

    if opts.export_json_index {
        let index_path = opts.destination.join("getall_index.json");
        let json = serde_json::to_string_pretty(&report.gathered)?;
        std::fs::write(&index_path, json).map_err(io_err(&index_path))?;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, content: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn gathers_only_selected_category() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("out");
        touch(&src.join("photo.jpg"), b"img");
        touch(&src.join("clip.mp4"), b"vid");
        touch(&src.join("notes.txt"), b"doc");

        let opts = GetAllOptions {
            sources: vec![src],
            destination: dest.clone(),
            categories: vec![FileCategory::Image],
            extra_extensions: vec![],
            mode: TransferMode::Copy,
            export_json_index: true,
        };
        let report = run_getall(&opts, |_, _, _| {}).unwrap();
        assert_eq!(report.gathered.len(), 1);
        assert!(dest.join("photo.jpg").exists());
        assert!(dest.join("getall_index.json").exists());
    }
}
