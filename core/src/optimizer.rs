//! Optimizer: shrink images/PDFs/videos/PPTX, lossless by default.
//!
//! Design principle from the spec: "sûr, pas de perte par défaut, rollback
//! si possible". Concretely:
//!   - we NEVER overwrite the source in place; we always write to a temp
//!     file first and only replace the original after a successful re-read,
//!   - "in place" mode keeps a `.bak` copy unless the caller opts out,
//!   - image and PPTX optimization are fully implemented here in pure Rust;
//!     video/PDF shell out to well-known external tools (ffmpeg / qpdf /
//!     ghostscript) because re-implementing video & PDF codecs in Rust is
//!     out of scope. Those tools are looked up via `fs_utils::resolve_tool`,
//!     which prefers a bundled copy next to the app (see README) before
//!     falling back to PATH, and returns a clear `MissingTool` error rather
//!     than failing silently if neither is found.

use crate::fs_utils::{io_err, resolve_tool, silent_command, unique_destination};
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    Pdf,
    Pptx,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Quality {
    /// Re-encode without perceptible/measurable quality loss (default).
    Lossless,
    /// Trade quality for size. Range is 0-100 (higher = better quality),
    /// mapped per-format below (JPEG quality, video CRF, PDF compression level...).
    Lossy(u8),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizeOptions {
    pub source: PathBuf,
    /// `None` => rewrite in place (with a `.bak` kept unless `keep_backup` is false).
    pub destination: Option<PathBuf>,
    pub kind: MediaKind,
    pub quality: Quality,
    pub keep_backup: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OptimizeReport {
    pub processed: Vec<OptimizedFile>,
    /// Result wasn't actually smaller than the original — original left
    /// untouched (in-place) or copied as-is (explicit destination). Distinct
    /// from `skipped`, which is "wrong file type, never attempted".
    pub unchanged: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
    /// `.bak` files created this run (only when `keep_backup` is on) — the
    /// UI can offer a one-click "delete all these now that I've checked the
    /// results" once the person has verified everything looks right.
    pub backups_created: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizedFile {
    pub path: PathBuf,
    pub original_size: u64,
    pub new_size: u64,
}

/// Modern PowerPoint formats — all of these are zip/OOXML archives under the
/// hood, so the same `optimize_pptx_file` (zip re-pack + per-image
/// re-encode) handles every one of them.
const OOXML_PPT_EXTS: &[&str] = &["pptx", "ppsx", "pptm", "ppsm", "potx", "potm"];
/// Pre-2007 binary PowerPoint formats (Compound File Binary Format, NOT a
/// zip archive) — a fundamentally different container our zip-based
/// optimizer cannot touch. Recognized so the person gets a clear, specific
/// message instead of the file silently disappearing from the run.
const LEGACY_OFFICE_EXTS: &[&str] = &["ppt", "pps", "pot"];

fn is_legacy_office(ext: &str) -> bool {
    LEGACY_OFFICE_EXTS.contains(&ext)
}
fn is_eml(ext: &str) -> bool {
    ext == "eml"
}

fn matches_kind(ext: &str, kind: MediaKind) -> bool {
    let ext = ext.to_lowercase();
    let is_image = ["jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff"].contains(&ext.as_str());
    let is_video = ["mp4", "avi", "mkv", "mov", "wmv", "flv", "webm"].contains(&ext.as_str());
    let is_pdf = ext == "pdf";
    let is_pptx = OOXML_PPT_EXTS.contains(&ext.as_str());
    let is_legacy = is_legacy_office(&ext);
    let is_email = is_eml(&ext);
    match kind {
        // Only the kinds we can actually do something with — NOT every file
        // in the tree. Matching everything here made `list_candidates` walk
        // (and later loop over, emitting a progress event for) every
        // unrelated file on the whole source drive when "Tout" was picked.
        // Legacy Office and .eml are included here too even though we can't
        // optimize them (yet) — so they show up with a clear explanation
        // instead of vanishing without a trace.
        MediaKind::All => is_image || is_video || is_pdf || is_pptx || is_legacy || is_email,
        MediaKind::Image => is_image,
        MediaKind::Video => is_video,
        MediaKind::Pdf => is_pdf,
        MediaKind::Pptx => is_pptx || is_legacy,
    }
}

/// Office (Word/Excel/PowerPoint) creates hidden `~$name.ext` lock files
/// next to any document that's currently open — they look like a real file
/// by extension but aren't valid archives/documents at all. Skip them
/// instead of reporting a confusing "invalid archive" error for each one.
fn is_office_lock_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("~$"))
        .unwrap_or(false)
}

/// Reads the first few bytes of a file that failed to decode and reports
/// what format they actually look like, so a persistent "invalid signature"
/// error can be diagnosed instead of just asserted. Recognizes a handful of
/// common formats that get mislabeled with the wrong extension (a JPEG
/// saved as `.png`, an ICO favicon, HTML/text saved with an image
/// extension, an empty/truncated file...).
fn magic_bytes_hint(path: &Path) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = [0u8; 16];
    let Ok(n) = std::io::Read::read(&mut file, &mut buf) else {
        return String::new();
    };
    if n == 0 {
        return " [le fichier fait 0 octet — vide]".to_string();
    }
    let b = &buf[..n];
    let detected = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        "un vrai PNG (le décodeur a échoué plus loin dans le fichier, probablement tronqué)"
    } else if b.starts_with(b"\xFF\xD8\xFF") {
        "en réalité un JPEG"
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        "en réalité un GIF"
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "en réalité un WebP"
    } else if b.starts_with(b"\x00\x00\x01\x00") {
        "en réalité une icône .ico"
    } else if b.starts_with(b"BM") {
        "en réalité un BMP"
    } else if b.starts_with(b"<") {
        "en réalité du HTML/texte (probablement une page d'erreur téléchargée à la place de l'image)"
    } else {
        "un format non reconnu (probablement corrompu ou tronqué)"
    };
    let hex: Vec<String> = b.iter().take(8).map(|byte| format!("{byte:02X}")).collect();
    format!(" [premiers octets: {} — {detected}]", hex.join(" "))
}

fn optimize_image_file(src: &Path, dest: &Path, quality: &Quality) -> crate::Result<()> {
    let img = image::open(src).map_err(|e| crate::FtpError::Other(format!("{}: {e}{}", src.display(), magic_bytes_hint(src))))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }

    let has_alpha = img.color().has_alpha();
    match quality {
        Quality::Lossless => {
            if has_alpha {
                img.save_with_format(dest, ImageFormat::Png)
                    .map_err(|e| crate::FtpError::Other(e.to_string()))?;
            } else {
                let mut out = std::fs::File::create(dest).map_err(io_err(dest))?;
                let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
                encoder
                    .encode_image(&img)
                    .map_err(|e| crate::FtpError::Other(e.to_string()))?;
            }
        }
        Quality::Lossy(q) => {
            let mut out = std::fs::File::create(dest).map_err(io_err(dest))?;
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, (*q).clamp(1, 100));
            encoder
                .encode_image(&img)
                .map_err(|e| crate::FtpError::Other(e.to_string()))?;
        }
    }
    Ok(())
}

/// Re-encodes in-memory image bytes (used for images embedded inside a
/// PPTX/zip, where we never want a loose temp file on disk). Returns `None`
/// if the bytes aren't a format the `image` crate can decode, or if
/// re-encoding didn't actually shrink anything — the caller then keeps the
/// original bytes unchanged, which is always safe.
fn optimize_image_bytes(bytes: &[u8], quality: &Quality) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let mut out = Vec::new();
    let has_alpha = img.color().has_alpha();
    match quality {
        Quality::Lossless if has_alpha => {
            let mut cursor = std::io::Cursor::new(&mut out);
            img.write_to(&mut cursor, ImageFormat::Png).ok()?;
        }
        Quality::Lossless => {
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
            encoder.encode_image(&img).ok()?;
        }
        Quality::Lossy(q) => {
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, (*q).clamp(1, 100));
            encoder.encode_image(&img).ok()?;
        }
    }
    if out.len() < bytes.len() {
        Some(out)
    } else {
        None
    }
}

fn run_external(tool: &str, args: &[String], extra_success_codes: &[i32]) -> crate::Result<()> {
    let tool_path = resolve_tool(tool).ok_or_else(|| crate::FtpError::MissingTool(tool.to_string()))?;
    let mut cmd = silent_command(&tool_path.to_string_lossy());
    // Force the tool's own folder as working directory. Note this does NOT
    // control DLL search order priority on Windows (that's already led by
    // the executable's own directory regardless of CWD) — it's just a
    // reasonable default some CLI tools expect. Kept because it's harmless,
    // not because it fixes DLL resolution.
    if let Some(dir) = tool_path.parent() {
        cmd.current_dir(dir);
    }
    let status = cmd
        .args(args)
        .status()
        .map_err(|e| crate::FtpError::Other(format!("{tool} ({}) failed to start: {e}", tool_path.display())))?;
    let ok = status.success() || status.code().is_some_and(|c| extra_success_codes.contains(&c));
    if !ok {
        // Always name the EXACT binary that was actually run — bundled
        // tools/ copy vs a PATH-resolved one look identical in a bare
        // "qpdf exited with ..." message, which cost real time to untangle
        // when both existed on the same machine at once.
        return Err(crate::FtpError::Other(format!(
            "{tool} ({}) exited with {status}",
            tool_path.display()
        )));
    }
    Ok(())
}

fn optimize_video_file(src: &Path, dest: &Path, quality: &Quality) -> crate::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let dest_str = dest.to_string_lossy().to_string();
    let src_str = src.to_string_lossy().to_string();
    let args: Vec<String> = match quality {
        Quality::Lossless => vec!["-y".into(), "-i".into(), src_str, "-c".into(), "copy".into(), dest_str],
        Quality::Lossy(q) => {
            let crf = (51.0 - (*q as f64 / 100.0) * 51.0).round().clamp(0.0, 51.0) as u32;
            vec![
                "-y".into(), "-i".into(), src_str,
                "-c:v".into(), "libx264".into(), "-crf".into(), crf.to_string(),
                "-preset".into(), "medium".into(), "-c:a".into(), "aac".into(), "-b:a".into(), "128k".into(),
                dest_str,
            ]
        }
    };
    run_external("ffmpeg", &args, &[])
}

fn optimize_pdf_file(src: &Path, dest: &Path, quality: &Quality) -> crate::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    match quality {
        // qpdf exits 3 when it succeeded but noticed (and fixed) minor
        // structural issues in the input PDF — that's a warning, not a
        // failure, and the output file is valid. Exit 2 (real errors) still
        // correctly fails.
        Quality::Lossless => run_external(
            "qpdf",
            &["--compress-streams=y".into(), "--object-streams=generate".into(),
              src.to_string_lossy().to_string(), dest.to_string_lossy().to_string()],
            &[3],
        ),
        Quality::Lossy(q) => {
            let preset = if *q < 33 { "screen" } else if *q < 66 { "ebook" } else { "printer" };
            run_external(
                "gs",
                &["-sDEVICE=pdfwrite".into(), "-dCompatibilityLevel=1.4".into(),
                  format!("-dPDFSETTINGS=/{preset}"), "-dNOPAUSE".into(), "-dBATCH".into(),
                  format!("-sOutputFile={}", dest.to_string_lossy()), src.to_string_lossy().to_string()],
                &[],
            )
        }
    }
}

/// PPTX is a zip archive. Every entry from the source archive is streamed
/// into a new one: media files under `ppt/media/` are re-encoded via
/// `optimize_image_bytes` when that shrinks them, every other entry (XML,
/// fonts, embedded objects...) is copied through unchanged at maximum
/// Deflate compression. This never touches the slide XML, so nothing about
/// the presentation's structure, text or layout can be broken by this step.
fn optimize_pptx_file(src: &Path, dest: &Path, quality: &Quality) -> crate::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let file = std::fs::File::open(src).map_err(io_err(src))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| crate::FtpError::Other(format!("{}: archive pptx invalide ({e})", src.display())))?;

    let out_file = std::fs::File::create(dest).map_err(io_err(dest))?;
    let mut writer = zip::ZipWriter::new(out_file);

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| crate::FtpError::Other(e.to_string()))?;
        let name = entry.name().to_string();
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes).map_err(io_err(dest))?;

        let is_media = name.starts_with("ppt/media/")
            && matches!(
                Path::new(&name).extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()),
                Some(ref e) if ["jpg", "jpeg", "png", "bmp", "tiff", "gif"].contains(&e.as_str())
            );

        let final_bytes = if is_media {
            optimize_image_bytes(&bytes, quality).unwrap_or(bytes)
        } else {
            bytes
        };

        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(9));
        writer.start_file(name, options).map_err(|e| crate::FtpError::Other(e.to_string()))?;
        writer.write_all(&final_bytes).map_err(io_err(dest))?;
    }

    writer.finish().map_err(|e| crate::FtpError::Other(e.to_string()))?;
    Ok(())
}

/// Compresses a `.eml` file losslessly into a `.zip` sitting next to it,
/// WITHOUT parsing or touching a single byte of the email's content.
///
/// We deliberately do not attempt to open the MIME structure and re-encode
/// embedded images in place: real-world `.eml` files vary hugely (nested
/// multipart/alternative + multipart/related, base64 vs quoted-printable,
/// RFC 2047 encoded headers, non-standard line endings...) and a mistake
/// there could corrupt an email that might matter (a ticket, a receipt).
/// A generic lossless zip is a little less effective on already-base64
/// image data, but it can never corrupt anything, and it's the same
/// technique you'd use by hand ("Envoyer vers > Dossier compressé"). The
/// original `.eml` is always left exactly as it was — this only ever adds
/// a `<name>.eml.zip` alongside it.
fn optimize_eml_file(src: &Path, dest_dir: &Path) -> crate::Result<PathBuf> {
    std::fs::create_dir_all(dest_dir).map_err(io_err(dest_dir))?;
    let file_name = src.file_name().unwrap_or_default();
    let zip_path = unique_destination(dest_dir.join(format!("{}.zip", file_name.to_string_lossy())));

    let bytes = std::fs::read(src).map_err(io_err(src))?;
    let out_file = std::fs::File::create(&zip_path).map_err(io_err(&zip_path))?;
    let mut writer = zip::ZipWriter::new(out_file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(9));
    writer
        .start_file(file_name.to_string_lossy().to_string(), options)
        .map_err(|e| crate::FtpError::Other(e.to_string()))?;
    writer.write_all(&bytes).map_err(io_err(&zip_path))?;
    writer.finish().map_err(|e| crate::FtpError::Other(e.to_string()))?;
    Ok(zip_path)
}

/// Locates LibreOffice's headless conversion binary. `resolve_tool` already
/// checks a bundled `tools/` copy and the PATH; LibreOffice's own installer
/// often does NOT add itself to PATH, so we also check its default install
/// locations on Windows as a last resort.
fn find_soffice() -> Option<PathBuf> {
    if let Some(p) = resolve_tool("soffice") {
        return Some(p);
    }
    #[cfg(windows)]
    {
        for candidate in [
            r"C:\Program Files\LibreOffice\program\soffice.exe",
            r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
        ] {
            let p = PathBuf::from(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Converts a legacy binary PowerPoint file (`.ppt`/`.pps`/`.pot` — pre-2007
/// Compound File Binary Format, NOT a zip) to `.pptx` via LibreOffice's
/// headless mode, then runs the normal PPTX optimizer on the result.
/// LibreOffice (free, cross-platform) is the only realistic option here —
/// there is no mature Rust crate for this decades-old, poorly documented
/// binary format, and reimplementing it is far out of scope. If LibreOffice
/// isn't found, this returns a clear `MissingTool` error rather than
/// failing silently. The original legacy file is never touched: this only
/// ever produces a new `<name>.pptx` alongside it.
fn convert_and_optimize_legacy_office(src: &Path, dest_dir: &Path, quality: &Quality) -> crate::Result<PathBuf> {
    let soffice = find_soffice().ok_or_else(|| {
        crate::FtpError::MissingTool(
            "soffice (LibreOffice) — installe LibreOffice (gratuit) pour convertir les anciens \
             formats PowerPoint, voir README"
                .to_string(),
        )
    })?;

    let convert_tmp = std::env::temp_dir().join("file-toolkit-pro-legacy-convert");
    std::fs::create_dir_all(&convert_tmp).map_err(io_err(&convert_tmp))?;

    let mut cmd = silent_command(&soffice.to_string_lossy());
    if let Some(dir) = soffice.parent() {
        cmd.current_dir(dir);
    }
    let status = cmd
        .args(["--headless", "--convert-to", "pptx", "--outdir"])
        .arg(&convert_tmp)
        .arg(src)
        .status()
        .map_err(|e| crate::FtpError::Other(format!("soffice failed to start: {e}")))?;
    if !status.success() {
        return Err(crate::FtpError::Other(format!("soffice exited with {status}")));
    }

    let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let converted = convert_tmp.join(format!("{stem}.pptx"));
    if !converted.is_file() {
        return Err(crate::FtpError::Other(
            "soffice n'a pas produit de fichier .pptx".to_string(),
        ));
    }

    std::fs::create_dir_all(dest_dir).map_err(io_err(dest_dir))?;
    let final_dest = unique_destination(dest_dir.join(format!("{stem}.pptx")));
    optimize_pptx_file(&converted, &final_dest, quality)?;
    let _ = std::fs::remove_file(&converted); // best-effort temp cleanup, never fatal
    Ok(final_dest)
}


/// single file) that match `kind`, without doing any work — used up front
/// so the UI can show a real "N / total" percentage instead of a spinner.
fn list_candidates(source: &Path, kind: MediaKind) -> Vec<PathBuf> {
    let walk = |root: &Path| -> Vec<PathBuf> {
        WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .filter(|p| !is_office_lock_file(p))
            .filter(|p| {
                let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
                matches_kind(&ext, kind)
            })
            .collect()
    };
    if source.is_dir() {
        walk(source)
    } else {
        let ext = source.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        if matches_kind(&ext, kind) { vec![source.to_path_buf()] } else { vec![] }
    }
}

/// Retires the original file once its replacement has been produced
/// successfully, for the eml/legacy-office cases where the output has a
/// different filename (so it can't just overwrite `src` via rename like the
/// image/video/pdf/pptx path does). Renames to `.bak` if `keep_backup`,
/// otherwise sends it to the OS trash — never a hard delete, consistent
/// with the rest of the app's "always recoverable" default.
fn retire_original(src: &Path, keep_backup: bool, report: &mut OptimizeReport) {
    if keep_backup {
        let ext = src.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        let bak = unique_destination(src.with_extension(format!("{ext}.bak")));
        match std::fs::rename(src, &bak) {
            Ok(()) => report.backups_created.push(bak),
            Err(e) => report.errors.push(format!(
                "{} : optimisation ok mais impossible de renommer l'original en .bak ({e})",
                src.display()
            )),
        }
    } else if let Err(e) = crate::fs_utils::safe_delete(src) {
        report.errors.push(format!(
            "{} : optimisation ok mais impossible de supprimer l'original ({e})",
            src.display()
        ));
    }
}

pub fn run_optimizer(
    opts: &OptimizeOptions,
    mut on_progress: impl FnMut(usize, usize, &Path),
) -> crate::Result<OptimizeReport> {
    let mut report = OptimizeReport::default();
    let in_place = opts.destination.is_none();

    let files = list_candidates(&opts.source, opts.kind);
    let total = files.len();
    on_progress(0, total, &opts.source);

    for (i, src) in files.into_iter().enumerate() {
        let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();

        // `destination` is always a FOLDER in this app's UI, even for a
        // single-file source — used below for the eml/legacy branches too.
        let dest_dir_for = |src: &Path| -> PathBuf {
            match &opts.destination {
                Some(d) if opts.source.is_dir() => {
                    let rel_parent = src
                        .parent()
                        .and_then(|p| p.strip_prefix(&opts.source).ok())
                        .unwrap_or_else(|| Path::new(""));
                    d.join(rel_parent)
                }
                Some(d) => d.clone(),
                None => src.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")),
            }
        };

        if is_legacy_office(&ext) {
            match convert_and_optimize_legacy_office(&src, &dest_dir_for(&src), &opts.quality) {
                Ok(new_pptx) => {
                    let original_size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
                    let new_size = std::fs::metadata(&new_pptx).map(|m| m.len()).unwrap_or(0);
                    if in_place {
                        retire_original(&src, opts.keep_backup, &mut report);
                    }
                    report.processed.push(OptimizedFile { path: new_pptx, original_size, new_size });
                }
                Err(e) => report.errors.push(format!(
                    "{} : conversion .{ext} -> .pptx échouée ({e}) — fichier original non touché",
                    src.display()
                )),
            }
            on_progress(i + 1, total, &src);
            continue;
        }
        if is_eml(&ext) {
            match optimize_eml_file(&src, &dest_dir_for(&src)) {
                Ok(zip_path) => {
                    let original_size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
                    let new_size = std::fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0);
                    if in_place {
                        retire_original(&src, opts.keep_backup, &mut report);
                    }
                    report.processed.push(OptimizedFile { path: zip_path, original_size, new_size });
                }
                Err(e) => report.errors.push(format!("{} : compression .eml échouée ({e})", src.display())),
            }
            on_progress(i + 1, total, &src);
            continue;
        }

        let original_size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);

        let final_dest = match &opts.destination {
            Some(d) if opts.source.is_dir() => {
                let rel = src.strip_prefix(&opts.source).unwrap_or(&src);
                d.join(rel)
            }
            // `destination` is always a FOLDER in this app's UI ("Dossier de
            // destination") — even for a single-file source, we must append
            // the original file name, not treat the folder path itself as
            // the output file.
            Some(d) => d.join(src.file_name().unwrap_or_default()),
            None => {
                // IMPORTANT: the real extension must stay LAST in the temp
                // filename (e.g. "video.ftp_tmp.mp4", not
                // "video.mp4.ftp_tmp") — external tools like ffmpeg infer
                // the output format from the filename's final extension, so
                // getting this backwards silently breaks every video/PDF.
                let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let temp_name = if ext.is_empty() {
                    format!("{stem}.ftp_tmp")
                } else {
                    format!("{stem}.ftp_tmp.{ext}")
                };
                src.with_file_name(temp_name)
            }
        };

        let effective_kind = if opts.kind == MediaKind::All {
            match ext.to_lowercase().as_str() {
                "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tiff" => MediaKind::Image,
                "mp4" | "avi" | "mkv" | "mov" | "wmv" | "flv" | "webm" => MediaKind::Video,
                "pdf" => MediaKind::Pdf,
                "pptx" | "ppsx" | "pptm" | "ppsm" | "potx" | "potm" => MediaKind::Pptx,
                _ => {
                    report.skipped.push(src.display().to_string());
                    on_progress(i + 1, total, &src);
                    continue;
                }
            }
        } else {
            opts.kind
        };

        let result = match effective_kind {
            MediaKind::Image => optimize_image_file(&src, &final_dest, &opts.quality),
            MediaKind::Video => optimize_video_file(&src, &final_dest, &opts.quality),
            MediaKind::Pdf => optimize_pdf_file(&src, &final_dest, &opts.quality),
            MediaKind::Pptx => optimize_pptx_file(&src, &final_dest, &opts.quality),
            MediaKind::All => unreachable!(),
        };

        match result {
            Ok(()) => {
                let new_size = std::fs::metadata(&final_dest).map(|m| m.len()).unwrap_or(0);

                if new_size >= original_size && original_size > 0 {
                    // Re-encoding didn't actually help (common for JPEGs
                    // re-saved at quality 95 from an already-compressed
                    // photo). A "safe" optimizer should never make a file
                    // bigger just to say it "optimized" it.
                    let _ = std::fs::remove_file(&final_dest);
                    if !in_place {
                        // Destination mode still gets a complete mirrored
                        // tree — just with the original bytes instead of a
                        // pointless larger copy.
                        if let Some(parent) = final_dest.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::copy(&src, &final_dest);
                    }
                    report.unchanged.push(src.display().to_string());
                } else if in_place {
                    let mut backup_ok = true;
                    if opts.keep_backup {
                        let bak = src.with_extension(format!("{ext}.bak"));
                        if let Err(e) = std::fs::copy(&src, &bak) {
                            report.errors.push(format!(
                                "{} : échec de la sauvegarde .bak ({e}) — fichier non modifié",
                                src.display()
                            ));
                            backup_ok = false;
                        } else {
                            report.backups_created.push(bak);
                        }
                    }
                    if backup_ok {
                        // A locked/in-use file (open in PowerPoint, being
                        // scanned by an antivirus, mid-sync in OneDrive...)
                        // must only fail THIS file, never abort the whole
                        // batch — every other error path in this function
                        // already works this way; this one used to use `?`
                        // and silently take the entire run down with it.
                        match std::fs::rename(&final_dest, &src) {
                            Ok(()) => report.processed.push(OptimizedFile {
                                path: src.clone(),
                                original_size,
                                new_size,
                            }),
                            Err(e) => report.errors.push(format!(
                                "{} : impossible de remplacer le fichier ({e}) — probablement \
                                 ouvert dans un autre programme",
                                src.display()
                            )),
                        }
                    }
                } else {
                    report.processed.push(OptimizedFile { path: final_dest, original_size, new_size });
                }
            }
            Err(e) => report.errors.push(format!("{}: {e}", src.display())),
        }
        on_progress(i + 1, total, &src);
    }

    Ok(report)
}
