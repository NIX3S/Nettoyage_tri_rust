use std::path::{Path, PathBuf};

fn main() {
    tauri_build::build();
    copy_tools_dir();
}

/// Copies `src-tauri/tools/` (qpdf/ffmpeg/adb, populated by
/// `scripts/vendor-tools.ps1`) next to the built executable — i.e. into
/// `target/debug/tools/` or `target/release/tools/`, exactly where
/// `resolve_tool()` (core/src/fs_utils.rs) looks for them at runtime for
/// `cargo run`/`cargo tauri dev` and plain `cargo build`.
///
/// This exists because a MANUAL copy step here is exactly the kind of thing
/// that goes stale and causes confusing bugs: a leftover file from an
/// earlier, incomplete manual copy (e.g. a lone `qpdf.exe` without its
/// companion DLLs) can sit in `target/debug/tools/` indefinitely and get
/// picked up by `resolve_tool()` before the real, complete copy — which is
/// exactly what happened once already. So instead: the destination is
/// wiped and freshly re-copied from `src-tauri/tools/` on every single
/// build, which makes that whole class of bug structurally impossible
/// going forward. `cargo tauri build` (real installers) doesn't need this —
/// that path already uses `bundle.resources` in `tauri.conf.json`.
fn copy_tools_dir() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_tools = manifest_dir.join("tools");
    println!("cargo:rerun-if-changed={}", src_tools.display());

    if !src_tools.is_dir() {
        return; // nothing vendored yet (vendor-tools.ps1 not run) — fine, resolve_tool() falls back to PATH
    }

    let Ok(out_dir) = std::env::var("OUT_DIR") else { return };
    // OUT_DIR is normally `<target>/<profile>/build/<pkg>-<hash>/out`;
    // three levels up from there is `<target>/<profile>/`, i.e. right next
    // to the final executable.
    let Some(profile_dir) = PathBuf::from(out_dir).ancestors().nth(3).map(Path::to_path_buf) else {
        return;
    };
    let dest_tools = profile_dir.join("tools");

    // Wipe first so a stale file from a previous manual copy can never
    // linger and shadow the real one (see doc comment above).
    let _ = std::fs::remove_dir_all(&dest_tools);
    if let Err(e) = copy_dir_recursive(&src_tools, &dest_tools) {
        println!("cargo:warning=couldn't copy tools/ to build output: {e}");
    }
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            std::fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}
