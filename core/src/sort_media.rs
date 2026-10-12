//! TriPhotosVideo: sorts photos/videos into `YYYY/MM` folders using EXIF date
//! when available, falling back to filesystem creation time, with a `NSP`
//! (Non Spécifié) bucket for anything we genuinely can't date. Ported and
//! generalized from tri.py.

use crate::fs_utils::transfer_file;
use crate::hash::sha256_file;
use crate::TransferMode;
use chrono::{DateTime, Local, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "tiff", "webp"];
const VIDEO_EXTS: &[&str] = &["mp4", "avi", "mkv", "mov", "wmv", "flv", "webm"];
const NSP_FOLDER: &str = "NSP";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortMediaOptions {
    /// If `videos_source` is None, photos and videos from `photos_source`
    /// are sorted into the same tree. If set, videos are read from there
    /// instead (spec: "un ou deux dossiers").
    pub photos_source: PathBuf,
    pub videos_source: Option<PathBuf>,
    /// Base destination, used when `photos_destination`/`videos_destination`
    /// aren't set.
    pub destination: PathBuf,
    /// Send sorted photos to a completely different location (e.g. a
    /// different drive: `E:\Photos`) instead of under `destination`.
    pub photos_destination: Option<PathBuf>,
    /// Same idea for videos (e.g. `F:\Videos`).
    pub videos_destination: Option<PathBuf>,
    pub mode: TransferMode,
    pub dedupe_by_hash: bool,
    /// Only used when `photos_destination`/`videos_destination` are both
    /// unset: keep photos and videos in separate top-level trees under the
    /// shared `destination` (destination/Photos/YYYY/MM vs destination/Videos/YYYY/MM).
    pub separate_photos_videos: bool,
    /// Optional custom prefix for the year folders, e.g. "Trip_" -> "Trip_2025".
    pub folder_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SortMediaReport {
    pub moved: Vec<PathBuf>,
    pub duplicates_skipped: usize,
    pub unclassified: usize,
    pub errors: Vec<String>,
}

fn extract_exif_date(path: &Path) -> Option<NaiveDateTime> {
    let file = std::fs::File::open(path).ok()?;
    let mut bufreader = std::io::BufReader::new(&file);
    let exif = exif::Reader::new().read_from_container(&mut bufreader).ok()?;
    let field = exif
        .get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::DateTime, exif::In::PRIMARY))?;
    let value = field.display_value().to_string();
    NaiveDateTime::parse_from_str(&value, "%Y:%m:%d %H:%M:%S").ok()
}

fn fallback_date(path: &Path) -> Option<NaiveDateTime> {
    let meta = std::fs::metadata(path).ok()?;
    let created = meta.created().or_else(|_| meta.modified()).ok()?;
    let dt: DateTime<Local> = created.into();
    Some(dt.naive_local())
}

fn resolve_date(path: &Path, is_image: bool) -> Option<NaiveDateTime> {
    if is_image {
        if let Some(d) = extract_exif_date(path) {
            return Some(d);
        }
    }
    fallback_date(path)
}

fn ext_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Which media-bearing sources to walk, tagged with whether that source is
/// the "photo" side or the "video" side (only matters when both sources are
/// given — otherwise a single mixed source accepts both kinds).
fn sources_for(opts: &SortMediaOptions) -> Vec<(PathBuf, bool)> {
    let mut sources = vec![(opts.photos_source.clone(), true)];
    if let Some(videos) = &opts.videos_source {
        sources.push((videos.clone(), false));
    }
    sources
}

fn is_relevant_file(path: &Path, treat_as_photo_source: bool, both_sources_given: bool) -> Option<bool> {
    let ext = ext_of(path);
    let is_image = IMAGE_EXTS.contains(&ext.as_str());
    let is_video = VIDEO_EXTS.contains(&ext.as_str());
    if !is_image && !is_video {
        return None;
    }
    if both_sources_given {
        if treat_as_photo_source && !is_image {
            return None;
        }
        if !treat_as_photo_source && !is_video {
            return None;
        }
    }
    Some(is_image)
}

pub fn run_sort_media(
    opts: &SortMediaOptions,
    mut on_progress: impl FnMut(usize, usize, &Path),
) -> crate::Result<SortMediaReport> {
    let mut report = SortMediaReport::default();
    let mut seen_hashes: HashSet<String> = HashSet::new();
    let sources = sources_for(opts);
    let both_given = opts.videos_source.is_some();

    // Pre-count so the UI shows a real percentage from the start.
    let mut total = 0usize;
    for (root, treat_as_photo) in &sources {
        if !root.is_dir() {
            continue;
        }
        total += WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| is_relevant_file(e.path(), *treat_as_photo, both_given).is_some())
            .count();
    }
    on_progress(0, total, &opts.destination);

    let mut count = 0usize;
    for (source_root, treat_as_photo_source) in sources {
        if !source_root.is_dir() {
            report.errors.push(format!("dossier introuvable: {}", source_root.display()));
            continue;
        }
        for entry in WalkDir::new(&source_root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(is_image) = is_relevant_file(path, treat_as_photo_source, both_given) else {
                continue;
            };

            if opts.dedupe_by_hash {
                match sha256_file(path) {
                    Ok(h) if seen_hashes.contains(&h) => {
                        report.duplicates_skipped += 1;
                        count += 1;
                        on_progress(count, total, path);
                        continue;
                    }
                    Ok(h) => {
                        seen_hashes.insert(h);
                    }
                    Err(e) => {
                        report.errors.push(format!("{}: {e}", path.display()));
                        count += 1;
                        on_progress(count, total, path);
                        continue;
                    }
                }
            }

            let date = resolve_date(path, is_image);

            // Destination resolution order: explicit per-type override (can
            // point anywhere, e.g. a different drive) > shared destination,
            // optionally split into Photos/Videos subfolders.
            let media_root = match (is_image, &opts.photos_destination, &opts.videos_destination) {
                (true, Some(d), _) => d.clone(),
                (false, _, Some(d)) => d.clone(),
                (true, None, _) if opts.separate_photos_videos => opts.destination.join("Photos"),
                (false, _, None) if opts.separate_photos_videos => opts.destination.join("Videos"),
                _ => opts.destination.clone(),
            };

            let target_dir = match date {
                Some(d) => {
                    let year = format!("{}{}", opts.folder_prefix, d.format("%Y"));
                    let month = d.format("%m").to_string();
                    media_root.join(year).join(month)
                }
                None => {
                    report.unclassified += 1;
                    media_root.join(NSP_FOLDER)
                }
            };

            let dest = target_dir.join(path.file_name().unwrap_or_default());
            match transfer_file(path, &dest, opts.mode) {
                Ok(final_dest) => report.moved.push(final_dest),
                Err(e) => report.errors.push(format!("{}: {e}", path.display())),
            }
            count += 1;
            on_progress(count, total, path);
        }
    }

    Ok(report)
}
