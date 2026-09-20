//! The pet window: its size, and remembering where the user put it.
//!
//! Transparency, decorations, always-on-top, shadow and `visibleOnAllWorkspaces` are all declared
//! in `tauri.conf.json`, not here — they have to be set when the window is created. The one
//! exception is [`follow_fullscreen`], which no cross-platform setting covers.

use std::sync::{Arc, Mutex};

use tauri::{
    LogicalPosition, LogicalSize, Manager, Monitor, PhysicalPosition, WebviewWindow, WindowEvent,
};

use crate::config::{Config, Saver};

/// Applies the configured size and the saved position, then keeps the position current.
pub fn configure(window: &WebviewWindow, config: &Arc<Mutex<Config>>, saver: Saver) {
    let (size, position, hidden) = {
        let config = config.lock().expect("config mutex poisoned");
        (
            config.size.edge(),
            (config.window.x, config.window.y),
            config.window.hidden,
        )
    };

    // Before anything else, so a pet the user put away does not flash across the screen on every
    // launch while it is being sized and positioned.
    if hidden {
        show(window, false);
    }

    follow_fullscreen(window);

    resize(window, size);

    // No saved position on a first run: leave the window wherever the OS put it rather than
    // guessing at a corner, and remember it as soon as the user moves it.
    if let (Some(x), Some(y)) = position
        && let Err(err) = window.set_position(LogicalPosition::new(x, y))
    {
        tracing::warn!("restoring window position: {err}");
    }

    // ⚠️ Unconditional, and not only when a position was restored. A saved position is a position
    // on *the display the user last had*, and that display may not be here any more — which is the
    // bug this exists for: a pet parked on an external monitor is restored to coordinates the
    // remaining display does not cover, and never appears.
    //
    // Startup is deliberately where this is guaranteed. A display lost while the pet is running is
    // usually caught too, because the OS moves the window and that arrives as a `Moved` — but it is
    // not guaranteed for a borderless always-on-top window, and rather than poll for the case that
    // slips through, the answer to a pet that has gone missing is to quit and start her again.
    rescue(window);

    watch_position(window, config.clone(), saver);
}

/// Lets the pet appear over a full-screen app, on macOS.
///
/// Three things have to be true, and only the first is a window setting:
///
/// **Admission.** `visibleOnAllWorkspaces` in `tauri.conf.json` covers ordinary Spaces — it is
/// `NSWindowCollectionBehaviorCanJoinAllSpaces`, and that is the whole of what tao sets. The Space
/// macOS builds for a green-button full-screen app is stricter: it admits only windows that have
/// *also* declared `FullScreenAuxiliary`, the window saying "I am not the one going full screen,
/// I am something that may be shown alongside whoever is".
///
/// **Being a panel.** Admission is not enough, and neither is raising the window level: AppKit
/// will not composite an ordinary `NSWindow` over another app's full-screen Space however high it
/// floats. Only an `NSPanel` gets that, which is why this goes through `tauri-nspanel` — it
/// reclasses the window tao made. Without it the pet is a *member* of the Space, which is exactly
/// the half-fixed state where she shows up in Mission Control and then fades out behind the
/// full-screen app.
///
/// **Not stealing focus.** `NonactivatingPanel`, because clicking a pet that activates the app
/// would throw the user out of the full-screen Space they are working in — which would make the
/// pet visible there and useless there in the same gesture.
///
/// The level is the menu bar's rather than higher: `NSPopUpMenuWindowLevel` and above is where
/// menus live, and the pet's own right-click menu has to open in front of her. `main.rs` making
/// the app an accessory is the last prerequisite — a regular app in the Dock does not float over
/// another app's full-screen Space whatever its window says.
#[cfg(target_os = "macos")]
fn follow_fullscreen(window: &WebviewWindow) {
    use tauri_nspanel::{CollectionBehavior, PanelLevel, StyleMask, WebviewWindowExt, tauri_panel};

    tauri_panel! {
        panel!(PetPanel {
            config: {
                can_become_key_window: false,
                can_become_main_window: false,
                is_floating_panel: true
            }
        })
    }

    let panel = match window.to_panel::<PetPanel>() {
        Ok(panel) => panel,
        Err(err) => {
            // Not fatal: the pet still works, she is just confined to the desktop she was
            // launched on, which is what every version before this did.
            tracing::warn!(
                "making the pet a panel: {err}; she will not show over full-screen apps"
            );
            return;
        }
    };

    panel.set_level(PanelLevel::Status.value());
    panel.set_collection_behavior(
        CollectionBehavior::new()
            .can_join_all_spaces()
            .full_screen_auxiliary()
            .value(),
    );
    // Additive rather than assigned: the style mask tao built carries the borderless-transparent
    // window the pet is drawn in, and replacing it wholesale is what AppKit rejects on a live
    // window.
    if let Err(err) = panel.add_style_mask(StyleMask::empty().nonactivating_panel().value()) {
        tracing::warn!("making the pet non-activating: {err}; clicking her may change Space");
    }
}

/// Nothing to do off macOS: full-screen Spaces are a macOS idea, and `visibleOnAllWorkspaces`
/// covers supported workspace switchers. Native Wayland still needs compositor rules.
#[cfg(not(target_os = "macos"))]
fn follow_fullscreen(_window: &WebviewWindow) {}

/// Puts the pet away, or brings her back.
///
/// Showing rescues her first. A pet hidden on an external display that was unplugged in the
/// meantime would otherwise come back to coordinates no remaining display covers — the same way a
/// pet parked there goes missing across a restart, which is what [`rescue`] exists for. Hiding is
/// the one gesture after which that can be true without any window event having said so.
pub fn show(window: &WebviewWindow, shown: bool) {
    let result = if shown {
        rescue(window);
        window.show()
    } else {
        window.hide()
    };
    if let Err(err) = result {
        tracing::warn!("setting pet visibility: {err}");
    }
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

        // A `Moved` this handler caused is not news, and re-entering `rescue` from inside it would
        // be. The corrected position still gets saved: moving the window emits another `Moved`,
        // and that one passes the check.
        if rescue(&handle) {
            return;
        }

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

/// Moves the pet back onto a display if it is not on one, and reports whether it had to.
///
/// Reads the position back from the window rather than trusting what was asked for: the platform
/// is free to place a window somewhere other than where it was told, and the point here is what
/// actually happened.
fn rescue(window: &WebviewWindow) -> bool {
    let Ok(monitors) = window.available_monitors() else {
        return false;
    };
    let areas: Vec<Rect> = monitors.iter().map(work_area).collect();
    // No monitors at all is a machine with the lid shut or a display change in progress. There is
    // nowhere to rescue the window *to*, and moving it on a guess would throw away a position that
    // is probably still good — leaving it alone means the next launch decides with real displays
    // to look at.
    if areas.is_empty() {
        return false;
    }

    let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return false;
    };
    let rect = Rect {
        x: position.x,
        y: position.y,
        w: size.width as i32,
        h: size.height as i32,
    };

    let Some(moved_to) = rehome(rect, &areas) else {
        return false;
    };

    tracing::info!(
        "pet was at {},{} with no display there; moving it to {},{}",
        rect.x,
        rect.y,
        moved_to.0,
        moved_to.1
    );
    if let Err(err) = window.set_position(PhysicalPosition::new(moved_to.0, moved_to.1)) {
        tracing::warn!("moving the window back on screen: {err}");
        return false;
    }
    true
}

/// A monitor's usable area, in the same physical, top-left-origin coordinates as a window's
/// `outer_position`. The *work* area rather than the whole monitor, so the pet is never parked
/// under the menu bar where it cannot be dragged out.
fn work_area(monitor: &Monitor) -> Rect {
    let area = monitor.work_area();
    Rect {
        x: area.position.x,
        y: area.position.y,
        w: area.size.width as i32,
        h: area.size.height as i32,
    }
}

/// A rectangle in physical pixels, top-left origin — the coordinate space both `outer_position`
/// and [`Monitor::work_area`] report in, on every platform this ships to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn centre(&self) -> (i64, i64) {
        (
            self.x as i64 + self.w as i64 / 2,
            self.y as i64 + self.h as i64 / 2,
        )
    }
}

/// The fraction of each edge that has to land on one work area for the window to count as
/// reachable.
///
/// A quarter, not all of it, because parking the pet half off the edge of the screen is a thing
/// people do with a desktop pet on purpose, and a rule that drags her back from a deliberate perch
/// is a worse bug than the one being fixed. Expressed as a fraction of the window rather than a
/// pixel count so it means the same thing on a Retina display as off it.
const VISIBLE_FRACTION: i32 = 4;

/// Where the window should be moved to, or `None` if it is already somewhere the user can reach.
///
/// The position is *clamped* into the nearest work area rather than centred on it, so a pet the
/// user kept at the right-hand edge of a second monitor comes back at the right-hand edge of the
/// one that is left. Which display was lost is not something we can ask about — the intent behind
/// the position is all that survives it.
fn rehome(window: Rect, areas: &[Rect]) -> Option<(i32, i32)> {
    if areas.iter().any(|area| reachable(window, *area)) {
        return None;
    }

    let (wx, wy) = window.centre();
    let target = areas.iter().min_by_key(|area| {
        let (ax, ay) = area.centre();
        (ax - wx).pow(2) + (ay - wy).pow(2)
    })?;

    // A window wider than the work area clamps to a negative range; `min` before `max` leaves it
    // at the top-left corner rather than inverted.
    Some((
        window
            .x
            .clamp(target.x, target.x + (target.w - window.w).max(0)),
        window
            .y
            .clamp(target.y, target.y + (target.h - window.h).max(0)),
    ))
}

fn reachable(window: Rect, area: Rect) -> bool {
    overlap(window.x, window.w, area.x, area.w) * VISIBLE_FRACTION >= window.w
        && overlap(window.y, window.h, area.y, area.h) * VISIBLE_FRACTION >= window.h
}

/// The length shared by two spans on one axis.
fn overlap(a: i32, a_len: i32, b: i32, b_len: i32) -> i32 {
    ((a + a_len).min(b + b_len) - a.max(b)).max(0)
}

/// The pet window, which `tauri.conf.json` labels `pet`.
pub fn pet(app: &tauri::AppHandle) -> Option<WebviewWindow> {
    let window = app.get_webview_window("pet");
    if window.is_none() {
        tracing::error!("no window labelled `pet`; check tauri.conf.json");
    }
    window
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in Retina display of the dev Mac: 2560x1600 physical, less the menu bar.
    const BUILTIN: Rect = Rect {
        x: 0,
        y: 50,
        w: 2560,
        h: 1550,
    };
    /// A second display placed to the right of it.
    const EXTERNAL: Rect = Rect {
        x: 2560,
        y: 0,
        w: 1920,
        h: 1080,
    };

    fn pet_at(x: i32, y: i32) -> Rect {
        Rect {
            x,
            y,
            w: 520,
            h: 520,
        }
    }

    #[test]
    fn a_window_on_a_display_is_left_alone() {
        assert_eq!(rehome(pet_at(300, 300), &[BUILTIN]), None);
        assert_eq!(rehome(pet_at(3000, 300), &[BUILTIN, EXTERNAL]), None);
    }

    /// The bug: the position was saved while a second display was attached, and it is not any more.
    #[test]
    fn a_window_on_a_display_that_left_comes_back() {
        let stranded = pet_at(4546, 528);
        let (x, y) = rehome(stranded, &[BUILTIN]).expect("should be rescued");
        assert!(reachable(pet_at(x, y), BUILTIN));
    }

    /// Clamping, not centring: the pet kept its corner.
    #[test]
    fn a_rescue_keeps_the_corner_the_user_chose() {
        let (x, y) = rehome(pet_at(4546, 60), &[BUILTIN]).expect("should be rescued");
        assert_eq!((x, y), (2040, 60), "top right of the remaining display");

        let (x, y) = rehome(pet_at(2600, 4000), &[BUILTIN]).expect("should be rescued");
        assert_eq!((x, y), (2040, 1080), "bottom right, since it was low down");
    }

    /// Deliberately perching the pet half off the edge has to keep working.
    #[test]
    fn a_window_perched_on_the_edge_is_left_alone() {
        assert_eq!(rehome(pet_at(-260, 300), &[BUILTIN]), None, "half off left");
        assert_eq!(
            rehome(pet_at(2300, 300), &[BUILTIN]),
            None,
            "half off right"
        );
        // A tenth of an edge showing is not enough to grab.
        assert!(rehome(pet_at(-470, 300), &[BUILTIN]).is_some());
    }

    /// A window straddling two displays overlaps neither entirely, and is fine.
    #[test]
    fn a_window_straddling_two_displays_is_left_alone() {
        assert_eq!(rehome(pet_at(2400, 300), &[BUILTIN, EXTERNAL]), None);
    }

    #[test]
    fn a_rescue_picks_the_nearest_of_several_displays() {
        let far_left = Rect {
            x: -1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        // Stranded off to the right, so it comes back to the rightmost display.
        let (x, _) = rehome(pet_at(9000, 300), &[far_left, EXTERNAL]).expect("should be rescued");
        assert_eq!(x, EXTERNAL.x + EXTERNAL.w - 520);
    }

    /// A pet configured larger than the display it is rescued onto still lands somewhere valid.
    #[test]
    fn a_window_bigger_than_the_work_area_lands_at_the_corner() {
        let tiny = Rect {
            x: 0,
            y: 0,
            w: 320,
            h: 240,
        };
        assert_eq!(rehome(pet_at(9000, 9000), &[tiny]), Some((0, 0)));
    }
}
