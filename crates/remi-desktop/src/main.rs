#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Wiring only. Everything with a decision in it lives in `remi-core`, in one of the modules
//! below, or in `ui/`.

mod bridge;
mod config;
mod window;

use std::sync::{Arc, Mutex};

use tauri::Manager;

use crate::bridge::{Bridge, StatePayload};
use crate::config::{Config, Saver};

/// Which renderer `ui/app.js` should mount. Asked for rather than baked into the URL so that
/// changing it is a config edit, not a rebuild — and Rust still never learns which one is running.
#[tauri::command]
fn renderer(config: tauri::State<'_, Arc<Mutex<Config>>>) -> &'static str {
    config
        .lock()
        .expect("config mutex poisoned")
        .renderer
        .as_query()
}

/// The pose to show right now, for a webview that has only just started listening.
#[tauri::command]
fn pet_state(bridge: tauri::State<'_, Bridge>) -> StatePayload {
    bridge.current()
}

fn main() {
    // `RUST_LOG=remi_desktop=debug,remi_core=debug` turns on the per-change state lines.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let loaded = Config::load();
    let selection = loaded.selection.to_selection();
    let connections = loaded.connections.clone();
    // Shared because two things write to it: the window, as the user drags the pet, and the
    // session menu, when it lands.
    let config = Arc::new(Mutex::new(loaded));
    let saver = Saver::start();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![renderer, pet_state])
        .setup(move |app| {
            app.manage(config.clone());

            let bridge = bridge::start(app.handle().clone(), &connections, selection);
            app.manage(bridge);

            if let Some(pet) = window::pet(app.handle()) {
                window::configure(&pet, &config, saver);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start the remi-desktop webview");
}
