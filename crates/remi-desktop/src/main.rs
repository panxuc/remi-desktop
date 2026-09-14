// Wiring only — window/menu/tray/bridge modules land at M3 (plan §5).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("failed to start the remi-desktop webview");
}
