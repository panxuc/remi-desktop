//! The pet window: its size, and remembering where the user put it.
//!
//! Transparency, decorations, always-on-top and shadow are all declared in `tauri.conf.json`, not
//! here — they have to be set when the window is created.

use std::sync::{Arc, Mutex};

use tauri::{LogicalPosition, LogicalSize, Manager, WebviewWindow, WindowEvent};

use crate::config::{Config, Saver};

/// Applies the configured size and the saved position, then keeps the position current.
pub fn configure(window: &WebviewWindow, config: &Arc<Mutex<Config>>, saver: Saver) {
    let (size, position) = {
        let config = config.lock().expect("config mutex poisoned");
        (config.size.edge(), (config.window.x, config.window.y))
    };

    resize(window, size);

    // No saved position on a first run: leave the window wherever the OS put it rather than
    // guessing at a corner, and remember it as soon as the user moves it.
    if let (Some(x), Some(y)) = position
        && let Err(err) = window.set_position(LogicalPosition::new(x, y))
    {
        tracing::warn!("restoring window position: {err}");
    }

    watch_position(window, config.clone(), saver);
}

/// Remi is square and the renderer refits itself to whatever it is given, so one number is the
/// whole of a resize. Logical pixels, so a size means the same thing on a Retina display as off it.
pub fn resize(window: &WebviewWindow, edge: f64) {
    if let Err(err) = window.set_size(LogicalSize::new(edge, edge)) {
        tracing::warn!("setting window size: {err}");
    }
}

/// Persists the window position as the user drags it. Debounced by the [`Saver`], because a drag
/// emits a `Moved` event per frame.
fn watch_position(window: &WebviewWindow, config: Arc<Mutex<Config>>, saver: Saver) {
    let handle = window.clone();
    window.on_window_event(move |event| {
        let WindowEvent::Moved(_) = event else {
            return;
        };
        // The event carries a physical position; ask the window for the logical one instead, so a
        // position saved on a Retina display still means the same place elsewhere.
        let Ok(position) = handle.outer_position() else {
            return;
        };
        let scale_factor = handle.scale_factor().unwrap_or(1.0);
        let position: LogicalPosition<f64> = position.to_logical(scale_factor);

        let snapshot = {
            let mut config = config.lock().expect("config mutex poisoned");
            config.window.x = Some(position.x);
            config.window.y = Some(position.y);
            config.clone()
        };
        saver.save(snapshot);
    });
}

/// The pet window, which `tauri.conf.json` labels `pet`.
pub fn pet(app: &tauri::AppHandle) -> Option<WebviewWindow> {
    let window = app.get_webview_window("pet");
    if window.is_none() {
        tracing::error!("no window labelled `pet`; check tauri.conf.json");
    }
    window
}
