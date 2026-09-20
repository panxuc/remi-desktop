//! The menu bar icon: a second way into the session menu, for when the pet is behind a window or
//! the user would rather not hunt for her.
//!
//! ⚠️ **The tray menu is pushed, not pulled.** This is the one surface where the OS, not us, is in
//! charge of the interaction. `tray-icon` dispatches the click and then immediately opens whatever
//! menu is already attached (`macos/mod.rs`, `on_tray_click`), while Tauri delivers that click to
//! us through the event loop rather than inline (`tauri::app`, where the tray handler does
//! `proxy.send_event`). So our handler runs a turn *after* the menu is on screen: by the time we
//! learn about a click, the user is already reading the answer.
//!
//! Everything here follows from that. The menu cannot be built on demand the way
//! [`crate::menu::popup`] builds it, so it is kept current ahead of the click instead — the
//! registry thread calls [`refresh`] whenever what the menu would say has changed, and never
//! merely because time passed. `menu.rs` is what makes that distinction possible by keeping
//! continuously varying text out of the rows in the first place.

use tauri::AppHandle;
use tauri::image::Image;
use tauri::tray::TrayIconBuilder;

use crate::menu;

/// The glyph, compiled in rather than bundled: a file beside the binary is one more thing to find
/// at runtime, and to get wrong differently in `cargo run` and in an `.app`.
///
/// Two of them, because only macOS tints a tray icon for its bar. There the glyph is flat black
/// and the system colours it (see [`create`]); everywhere else the same black shape would sit
/// unaltered on a taskbar that is dark by default on both Windows 11 and Breeze, so those
/// platforms get the ornament's own pale-fill-and-navy-outline instead, which shows up on a panel
/// of either shade. `icons/tray/tray-color.svg` explains the colours.
#[cfg(target_os = "macos")]
const ICON: &[u8] = include_bytes!("../icons/tray/tray-44.png");
#[cfg(not(target_os = "macos"))]
const ICON: &[u8] = include_bytes!("../icons/tray/tray-color-44.png");

/// Named so [`refresh`] can find the tray again without threading a handle through the registry.
const ID: &str = "remi";

/// Puts Remi in the menu bar. Called once, from `setup`.
///
/// Failure is not fatal: the pet herself still carries the same menu on a right-click, so a missing
/// tray costs discoverability rather than the way out of the app.
pub fn create(app: &AppHandle) {
    let icon = match Image::from_bytes(ICON) {
        Ok(icon) => icon,
        Err(err) => return tracing::error!("decoding the menu bar icon: {err}"),
    };

    // AppIndicator on Linux needs a menu at creation time to expose the tray icon.
    let initial_menu = match menu::build(app) {
        Ok(menu) => menu,
        Err(err) => return tracing::error!("building the menu bar menu: {err}"),
    };
    let built = TrayIconBuilder::with_id(ID)
        .menu(&initial_menu)
        .icon(icon)
        // On macOS the glyph is one flat black shape on transparency precisely so this can be
        // true: as a template it is tinted to match the bar, correct in light and dark and dimmed
        // when the bar is inactive. Elsewhere the flag is ignored outright rather than honoured
        // differently, so it is declared false to match the coloured icon those platforms load —
        // claiming template of an icon nobody will treat as one only misleads the next reader.
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Remi")
        .build(app);

    match built {
        Ok(_) => {}
        Err(err) => tracing::error!("creating the menu bar icon: {err}"),
    }
}

/// Rebuilds the tray's menu and hands the replacement to the OS.
///
/// ⚠️ **Main thread only**, and unlike [`crate::menu::popup`] that is a requirement rather than a
/// prohibition: building a menu marshals to the main thread and waits for it, which is free when
/// already there (`tauri-runtime-wry`'s `send_user_message` runs it inline) and a round trip when
/// not. Nothing here opens a menu, so none of `popup`'s nested-event-loop hazard applies.
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(ID) else {
        return;
    };
    let menu = match menu::build(app) {
        Ok(menu) => menu,
        Err(err) => return tracing::error!("building the menu bar menu: {err}"),
    };
    if let Err(err) = tray.set_menu(Some(menu)) {
        tracing::error!("attaching the menu bar menu: {err}");
    }
}
