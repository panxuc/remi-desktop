//! The pet window: size, remembered position, click-through (plan §5.1, §5.4).
//!
//! Transparency, decorations, always-on-top and shadow are all declared in `tauri.conf.json`, not
//! here — they have to be set when the window is created.

use std::sync::{Arc, Mutex};

use tauri::{LogicalPosition, LogicalSize, Manager, WebviewWindow, WindowEvent};

use crate::config::{Config, Saver};

/// The art is 360×360 (brief §2.1) and `tauri.conf.json` opens the window at that size; `scale`
/// multiplies it.
const BASE_SIZE: f64 = 360.0;

/// Guards against a config that would make Remi a dot or fill the screen. Not a taste judgement —
/// a `scale` of 0 produces a window the user can neither see nor grab to fix.
const SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.25..=4.0;

/// Applies the saved size, position and click-through, then keeps the position current.
pub fn configure(window: &WebviewWindow, config: &Arc<Mutex<Config>>, saver: Saver) {
    let (scale, position, click_through) = {
        let config = config.lock().expect("config mutex poisoned");
        (
            config.scale.clamp(*SCALE_RANGE.start(), *SCALE_RANGE.end()),
            (config.window.x, config.window.y),
            config.click_through,
        )
    };

    let size = BASE_SIZE * f64::from(scale);
    if let Err(err) = window.set_size(LogicalSize::new(size, size)) {
        tracing::warn!("setting window size: {err}");
    }

    // No saved position on a first run: leave the window wherever the OS put it rather than
    // guessing at a corner, and remember it as soon as the user moves it.
    if let (Some(x), Some(y)) = position
        && let Err(err) = window.set_position(LogicalPosition::new(x, y))
    {
        tracing::warn!("restoring window position: {err}");
    }

    // ⚠️ Only ever from config. Turning this on leaves the window unable to receive the
    // right-click that would turn it off, and the tray — the way back (plan §5.2) — does not
    // exist yet, so nothing in the app may enable it on the user's behalf.
    if click_through && let Err(err) = window.set_ignore_cursor_events(true) {
        tracing::warn!("enabling click-through: {err}");
    }

    watch_position(window, config.clone(), saver);
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
