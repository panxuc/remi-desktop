//! The session menu: what right-clicking Remi opens.
//!
//! It is built fresh on every popup rather than kept and mutated. A native menu cannot change
//! while it is open, and every row of this one is time-dependent — a pose that fades, and how long
//! ago the session was last heard from — so a menu older than the click that opened it would be
//! showing something that is no longer true.
//!
//! Building the menu and acting on a click are separate from where the click came from, because
//! the tray will carry this identical menu (plan §5.2).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use remi_core::record::{HarnessId, SessionId};
use remi_core::registry::{ConnectionStatus, Selection, SessionEntry, SessionKey};
use remi_core::source::ConnectionId;
use remi_core::state::PetState;
use tauri::menu::{
    CheckMenuItemBuilder, ContextMenu, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder,
};
use tauri::{AppHandle, Manager, Wry};

use crate::bridge::{Bridge, MenuSnapshot, Message};
use crate::config::{Config, NamedSize, Saver, SelectionConfig, Size};
use crate::window;

/// Item ids. A session's id carries its whole [`SessionKey`], so acting on a click needs no second
/// look into a registry that may have moved on since the menu was built — and a click on a session
/// that has since ended pins it and shows it `Offline`, which is the honest answer.
///
/// The connection name comes last because it is the only part the user names freely, and so the
/// only one that may contain a `:`; harness and session ids cannot (remi-core `record.rs`).
const SESSION: &str = "session:";
const FOLLOW: &str = "follow-newest";
const SIZE: &str = "size:";
const QUIT: &str = "quit";

/// Where a session's name is cut. Titles arrive at up to 80 characters, and a menu as wide as that
/// covers a good part of the screen for the seconds it is open.
const NAME_MAX: usize = 44;

/// Where a connection's failure reason is cut. These come from ssh and from broker clients and
/// have no length limit at all.
const REASON_MAX: usize = 60;

/// What clicking an item does. Parsed from the id alone, so nothing has to be remembered between
/// building a menu and the click that closes it.
enum Action {
    /// Follow whichever session is newest — [`Selection::Auto`].
    Follow,
    /// Show this session, and keep showing it after it ends.
    Show(SessionKey),
    Resize(NamedSize),
    Quit,
}

/// Opens the session menu at the cursor.
///
/// ⚠️ **Never call this on the main thread.** Creating a menu marshals to the main thread and
/// blocks until it answers, and on macOS the popup then runs a nested event loop there until the
/// menu is dismissed — so from the main thread it deadlocks rather than failing. The `context_menu`
/// command is declared `#[tauri::command(async)]`, which runs it on a worker thread, for exactly
/// this reason.
pub fn popup(app: &AppHandle) {
    let Some(pet) = window::pet(app) else {
        return;
    };
    let menu = match build(app) {
        Ok(menu) => menu,
        Err(err) => return tracing::error!("building the session menu: {err}"),
    };
    if let Err(err) = menu.popup(pet.as_ref().window()) {
        tracing::error!("opening the session menu: {err}");
    }
}

/// The menu as the user sees it: every session the pet has heard of, then how to follow the newest
/// one instead, then the pet's own settings.
///
/// Sessions are listed flat and labelled `<connection> · <name>`, rather than nested under their
/// connection, because the list is short and one level reads faster than three. The harness is
/// named only where a connection is running more than one, where it is the only thing telling two
/// otherwise identical rows apart.
fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let MenuSnapshot {
        connections,
        selection,
    } = app.state::<Bridge>().snapshot();
    let size = app
        .state::<Arc<Mutex<Config>>>()
        .lock()
        .expect("config mutex poisoned")
        .size;

    let mut menu = MenuBuilder::new(app);
    for connection in &connections {
        // A connection with nothing to say is still listed: a host that is down or idle staying
        // visible is the difference between "nothing is happening there" and "I forgot about it".
        if let ConnectionStatus::Lost { reason } = &connection.status {
            let text = format!(
                "{} — disconnected: {}",
                connection.connection,
                cut(reason, REASON_MAX)
            );
            menu = menu.item(&MenuItemBuilder::new(text).enabled(false).build(app)?);
        } else if connection.harnesses.is_empty() {
            let text = format!("{} — no sessions", connection.connection);
            menu = menu.item(&MenuItemBuilder::new(text).enabled(false).build(app)?);
        }

        let name_harness = connection.harnesses.len() > 1;
        for harness in &connection.harnesses {
            for entry in &harness.sessions {
                let item = CheckMenuItemBuilder::with_id(
                    session_id(&entry.key),
                    row(&connection.connection, entry, name_harness),
                )
                .checked(matches!(&selection, Selection::Pinned(key) if *key == entry.key))
                .build(app)?;
                menu = menu.item(&item);
            }
        }
    }

    let follow = CheckMenuItemBuilder::with_id(FOLLOW, "Follow most recent")
        .checked(selection == Selection::Auto)
        .build(app)?;

    let mut sizes = SubmenuBuilder::new(app, "Size");
    for (named, text) in [
        (NamedSize::Small, "Small"),
        (NamedSize::Medium, "Medium"),
        (NamedSize::Large, "Large"),
    ] {
        // A hand-written pixel count in the config is not one of these three, so none is ticked —
        // which is the truth, and picking one from here is how the user goes back to a named size.
        let item = CheckMenuItemBuilder::with_id(format!("{SIZE}{}", name_of(named)), text)
            .checked(size == Size::Named(named))
            .build(app)?;
        sizes = sizes.item(&item);
    }

    menu.separator()
        .item(&follow)
        .separator()
        .item(&sizes.build()?)
        .text(QUIT, "Quit Remi")
        .build()
}

/// Acts on a click. Called for every menu event in the app, including the tray's once it exists,
/// which is why an id it does not recognise is a warning rather than a panic.
pub fn on_event(app: &AppHandle, id: &str) {
    let Some(action) = action(id) else {
        return tracing::warn!("ignoring unrecognised menu item {id:?}");
    };
    match action {
        Action::Follow => select(app, Selection::Auto),
        Action::Show(key) => select(app, Selection::Pinned(key)),
        Action::Resize(named) => resize(app, named),
        // The pet has no window to close and no dock icon, so this is the only way out.
        Action::Quit => app.exit(0),
    }
}

/// Changes which session Remi shows, and remembers it. The registry decides what that means for
/// the pose; this only tells it, and the change reaches the webview through the usual event.
fn select(app: &AppHandle, selection: Selection) {
    tracing::debug!(?selection, "selection changed");
    app.state::<Bridge>()
        .send(Message::Select(selection.clone()));
    save(app, |config| {
        config.selection = SelectionConfig::from_selection(&selection)
    });
}

/// Resizes the window and remembers the new size. The renderer refits itself to whatever it is
/// given, so nothing has to be told about this.
fn resize(app: &AppHandle, named: NamedSize) {
    let size = Size::Named(named);
    save(app, |config| config.size = size);
    if let Some(pet) = window::pet(app) {
        window::resize(&pet, size.edge());
    }
}

/// Applies a change to the live config and queues the file write. Both halves matter: the live
/// copy is what the next menu is built from, and the file is what the next launch reads.
fn save(app: &AppHandle, change: impl FnOnce(&mut Config)) {
    let snapshot = {
        let config = app.state::<Arc<Mutex<Config>>>();
        let mut config = config.lock().expect("config mutex poisoned");
        change(&mut config);
        config.clone()
    };
    app.state::<Saver>().save(snapshot);
}

fn action(id: &str) -> Option<Action> {
    if id == FOLLOW {
        return Some(Action::Follow);
    }
    if id == QUIT {
        return Some(Action::Quit);
    }
    if let Some(name) = id.strip_prefix(SIZE) {
        return match name {
            "small" => Some(Action::Resize(NamedSize::Small)),
            "medium" => Some(Action::Resize(NamedSize::Medium)),
            "large" => Some(Action::Resize(NamedSize::Large)),
            _ => None,
        };
    }

    // The connection is whatever is left after the two parts that cannot contain a `:`, so a
    // connection named with one still round-trips.
    let mut parts = id.strip_prefix(SESSION)?.splitn(3, ':');
    let harness = HarnessId::new(parts.next()?).ok()?;
    let session = SessionId::new(parts.next()?).ok()?;
    let connection = ConnectionId::new(parts.next()?);
    Some(Action::Show(SessionKey {
        connection,
        harness,
        session,
    }))
}

fn session_id(key: &SessionKey) -> String {
    format!(
        "{SESSION}{}:{}:{}",
        key.harness, key.session, key.connection
    )
}

/// One session's row: which machine it is on, what to call it, what it is doing, and when that was
/// last true. The last part is what gives away a session that died rather than ended.
fn row(connection: &ConnectionId, entry: &SessionEntry, name_harness: bool) -> String {
    let harness = if name_harness {
        format!(" ({})", entry.key.harness)
    } else {
        String::new()
    };
    format!(
        "{connection} · {name}{harness} — {pose}, {age}",
        name = cut(&entry.label, NAME_MAX),
        pose = pose(entry.pose),
        age = age(entry.last_heard),
    )
}

/// How a pose reads in the menu. The state names, except that `waiting_for_input` spelled out
/// makes every other row look terse by comparison.
fn pose(state: PetState) -> &'static str {
    match state {
        PetState::Thinking => "thinking",
        PetState::Viewing => "viewing",
        PetState::Writing => "writing",
        PetState::Replying => "replying",
        PetState::WaitingForInput => "waiting",
        PetState::Proud => "done",
        PetState::Idle => "idle",
        PetState::Offline => "offline",
    }
}

fn name_of(size: NamedSize) -> &'static str {
    match size {
        NamedSize::Small => "small",
        NamedSize::Medium => "medium",
        NamedSize::Large => "large",
    }
}

/// Roughly how long ago, at one significant figure. Nothing here is worth a second one: the point
/// of the number is telling seconds from minutes from hours.
fn age(since: Duration) -> String {
    match since.as_secs() {
        // Under the 1 Hz tick and the coalescing window, so anything smaller would be noise.
        secs if secs < 5 => "just now".to_owned(),
        secs if secs < 60 => format!("{secs}s"),
        secs if secs < 60 * 60 => format!("{}m", secs / 60),
        secs if secs < 60 * 60 * 24 => format!("{}h", secs / (60 * 60)),
        secs => format!("{}d", secs / (60 * 60 * 24)),
    }
}

/// Truncates on a character boundary, since titles and ssh errors are both arbitrary UTF-8.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    text.chars().take(max - 1).chain(['…']).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(connection: &str, harness: &str, session: &str) -> SessionKey {
        SessionKey {
            connection: ConnectionId::new(connection),
            harness: HarnessId::new(harness).unwrap(),
            session: SessionId::new(session).unwrap(),
        }
    }

    #[test]
    fn a_session_key_survives_the_round_trip_through_an_item_id() {
        // A connection name is the user's, so it is the one part that may hold the separator.
        let original = key("home:1", "claude-code", "a1b2_c3-d4");
        let id = session_id(&original);
        let Some(Action::Show(parsed)) = action(&id) else {
            panic!("{id} did not parse back to a session");
        };
        assert_eq!(parsed, original);
    }

    #[test]
    fn the_fixed_items_parse_and_nothing_else_does() {
        assert!(matches!(action(FOLLOW), Some(Action::Follow)));
        assert!(matches!(action(QUIT), Some(Action::Quit)));
        assert!(matches!(
            action("size:large"),
            Some(Action::Resize(NamedSize::Large))
        ));
        // Ids from some other menu, and ids this build cannot make sense of.
        assert!(action("size:enormous").is_none());
        assert!(action("session:Claude Code:x:local").is_none());
        assert!(action("something-else").is_none());
    }

    #[test]
    fn ages_read_as_one_unit() {
        assert_eq!(age(Duration::from_secs(2)), "just now");
        assert_eq!(age(Duration::from_secs(42)), "42s");
        assert_eq!(age(Duration::from_secs(60 * 4 + 30)), "4m");
        assert_eq!(age(Duration::from_secs(60 * 60 * 3)), "3h");
        assert_eq!(age(Duration::from_secs(60 * 60 * 24 * 2)), "2d");
    }

    #[test]
    fn a_long_name_is_cut_on_a_character_boundary() {
        assert_eq!(cut("short", 10), "short");
        assert_eq!(cut("ありがとうございます", 4), "ありが…");
    }
}
