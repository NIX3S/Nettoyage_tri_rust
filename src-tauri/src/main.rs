// File Toolkit Pro — Tauri entry point.
//
// WHY TAURI (see README for the full rationale):
//   - single Rust binary embedding a native OS webview => no bundled Chromium,
//     small final executable on all three OSes,
//   - the UI is plain HTML/CSS/JS, which is the most reliable way to get a
//     true glassmorphism look (backdrop-filter blur, CSS transitions) that
//     renders identically on Windows/macOS/Linux — a native immediate-mode
//     GUI (egui/iced) would fight us on that specific look,
//   - `core` (this app's business logic) stays a plain Rust lib with zero
//     Tauri dependency, so commands here are thin, testable glue.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use commands::AppState;
use std::sync::Mutex;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(Mutex::new(AppState::default()))
        .invoke_handler(tauri::generate_handler![
            commands::pick_folder_cmd,
            commands::pick_text_file_cmd,
            commands::pick_save_text_file_cmd,
            commands::read_text_file_cmd,
            commands::write_text_file_cmd,
            commands::compare_folders_cmd,
            commands::auto_update_cmd,
            commands::reveal_in_file_manager_cmd,
            commands::find_duplicates_cmd,
            commands::delete_duplicates_cmd,
            commands::run_getall_cmd,
            commands::run_sort_media_cmd,
            commands::run_optimizer_cmd,
            commands::set_adb_path_cmd,
            commands::list_android_devices_cmd,
            commands::run_photo_pull_cmd,
        ])
        .run(tauri::generate_context!())
        .expect("error while running File Toolkit Pro");
}
