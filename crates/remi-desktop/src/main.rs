#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Wiring only. Everything with a decision in it lives in `remi-core`, in one of the modules
//! below, or in `ui/`.

mod bridge;
mod config;
mod menu;
mod window;

use std::sync::{Arc, Mutex};

use tauri::{LogicalPosition, Manager};

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

/// Opens the session menu, which `ui/app.js` asks for when the pet is right-clicked. A webview
/// cannot draw a native menu, so the gesture is JavaScript's and the menu is Rust's.
///
/// ⚠️ `(async)` is load-bearing, not decoration: it runs this on a worker thread, and building or
/// popping up a menu from the main thread deadlocks against the event loop it is waiting on
/// (`menu::popup`). Taking the handle by value rather than `tauri::State` is part of the same
/// thing — an off-thread command cannot borrow from the request.
///
/// `x` and `y` are where the click landed, in logical pixels from the window's top-left.
#[tauri::command(async)]
fn context_menu(app: tauri::AppHandle, x: f64, y: f64) {
    menu::popup(&app, LogicalPosition::new(x, y));
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
        .invoke_handler(tauri::generate_handler![renderer, pet_state, context_menu])
        .setup(move |app| {
            // `LSUIElement` in Info.plist is not enough on its own: tao sets the activation
            // policy to `Regular` at startup, which overrides the plist key and puts the Dock
            // icon back. The plist is still worth keeping — it decides how the bundle is
            // presented *before* this line runs — but this is what actually holds.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            app.manage(config.clone());
            app.manage(saver.clone());

            let bridge = bridge::start(app.handle().clone(), &connections, selection);
            app.manage(bridge);

            // Every menu event in the app arrives here, which is what will let the tray carry the
            // same menu without carrying a second handler for it.
            app.on_menu_event(|app, event| menu::on_event(app, event.id().as_ref()));

            if let Some(pet) = window::pet(app.handle()) {
                window::configure(&pet, &config, saver);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start the remi-desktop webview");
}
