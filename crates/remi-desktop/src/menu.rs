//! The session menu: what right-clicking Remi opens.
//!
//! It is built fresh on every popup rather than kept and mutated. A native menu cannot change
//! while it is open, so a menu older than the click that opened it would be showing something that
//! is no longer true.
//!
//! Building the menu and acting on a click are separate from where the click came from, because
//! the tray carries this identical menu (`tray.rs`).
//!
//! ⚠️ **Nothing in a row may vary continuously.** The right-click menu can afford it — it is built
//! for the click that opens it — but the tray's cannot: macOS is handed a finished menu and opens
//! it later without asking us again, so whatever a row said when it was built is what the user
//! reads. Rows therefore carry only what changes in steps — a pose, and [`STALE_AFTER`] in place
//! of the exact age they used to show — which is also what lets [`settled`] tell a menu that needs
//! rebuilding from one that does not.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use remi_core::record::{HarnessId, SessionId};
use remi_core::registry::{ConnectionMenu, ConnectionStatus, Selection, SessionEntry, SessionKey};
use remi_core::source::ConnectionId;
use remi_core::state::PetState;
use tauri::menu::{
    CheckMenuItem, CheckMenuItemBuilder, ContextMenu, Menu, MenuBuilder, MenuItem, MenuItemBuilder,
    Submenu, SubmenuBuilder,
};
use tauri::{AppHandle, LogicalPosition, Manager, Wry};

use crate::bridge::{Bridge, MenuSnapshot, Message};
use crate::config::{Config, Connection, ConnectionKind, NamedSize, Saver, SelectionConfig, Size};
use crate::window;

/// Item ids. A session's id carries its whole [`SessionKey`], so acting on a click needs no second
/// look into a registry that may have moved on since the menu was built — and a click on a session
/// that has since ended pins it and shows it `Offline`, which is the honest answer.
///
/// The connection name comes last because it is the only part the user names freely, and so the
/// only one that may contain a `:`; harness and session ids cannot (remi-core `record.rs`).
const SESSION: &str = "session:";
const CONNECT: &str = "connect:";
const DISCONNECT: &str = "disconnect:";
const FOLLOW: &str = "follow-newest";
const SIZE: &str = "size:";
// Two ids rather than one that toggles: an id says what the user asked for, and a menu built
// a moment before the click could have been describing a visibility that has since changed.
const SHOW: &str = "show";
const HIDE: &str = "hide";
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
    /// Start watching this machine, and remember it for next time.
    Connect(ConnectionId),
    /// Stop watching this machine, and forget it.
    Disconnect(ConnectionId),
    Resize(NamedSize),
    /// Put the pet away, or bring her back. Reachable from the menu bar either way, which is
    /// the only reason hiding her is safe to offer at all.
    Visible(bool),
    Quit,
}

/// Opens the session menu at `at`, a point inside the pet window.
///
/// ⚠️ Always with a position, never `popup` and its "wherever the cursor is". On Linux that asks
/// GTK to anchor the menu to the screen's root window at the global pointer position, and Wayland
/// has neither: the menu has no parent to attach to, so it is mapped as a window of its own and
/// the compositor puts it in the middle of the screen. Given a position, the menu is anchored to
/// the pet window instead, which is what Wayland requires of a popup.
///
/// ⚠️ **Never call this on the main thread.** Creating a menu marshals to the main thread and
/// blocks until it answers, and on macOS the popup then runs a nested event loop there until the
/// menu is dismissed — so from the main thread it deadlocks rather than failing. The `context_menu`
/// command is declared `#[tauri::command(async)]`, which runs it on a worker thread, for exactly
/// this reason.
pub fn popup(app: &AppHandle, at: LogicalPosition<f64>) {
    let Some(pet) = window::pet(app) else {
        return;
    };
    let menu = match build(app) {
        Ok(menu) => menu,
        Err(err) => return tracing::error!("building the session menu: {err}"),
    };
    if let Err(err) = menu.popup_at(pet.as_ref().window(), at) {
        tracing::error!("opening the session menu: {err}");
    }
}

/// The menu as the user sees it: this machine's sessions, then every machine the pet is watching
/// as a row that opens, then the hosts it is not, then how to follow the newest session, then the
/// pet's own settings.
///
/// **This machine's sessions are flat and first.** It is always connected, needs no configuring,
/// and is where most sessions are — and one level reads faster than two for the rows the user
/// actually picks from.
///
/// **Every watched machine is a submenu**, because a host is something to act on — disconnect,
/// cancel a connection — and not merely a heading. What nesting costs is the glance that says
/// which machine wants attention, so the host's own row pays it back: it carries how many sessions
/// are on it and how many of those are waiting.
///
/// **Everything the pet is *not* watching is one level further in**, under Connect to a host…,
/// because the top level is for what is happening and a host nobody is connected to is not that.
/// It is also what stops a `~/.ssh/config` with nine hosts in it burying the sessions.
pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let MenuSnapshot {
        connections,
        selection,
        current,
    } = app.state::<Bridge>().snapshot();
    let (size, local) = {
        let config = app.state::<Arc<Mutex<Config>>>();
        let config = config.lock().expect("config mutex poisoned");
        let local: Vec<String> = config
            .connections
            .iter()
            .filter(|connection| connection.kind == ConnectionKind::Local)
            .map(|connection| connection.name.clone())
            .collect();
        (config.size, local)
    };
    let is_local = |connection: &ConnectionMenu| {
        local
            .iter()
            .any(|name| name == connection.connection.as_str())
    };

    let mut menu = MenuBuilder::new(app);

    for connection in connections.iter().filter(|it| is_local(it)) {
        if connection.harnesses.is_empty() {
            let text = format!("{} — no sessions", connection.connection);
            menu = menu.item(&MenuItemBuilder::new(text).enabled(false).build(app)?);
        }
        for item in sessions(app, connection, &selection, Some(&connection.connection))? {
            menu = menu.item(&item);
        }
    }

    // Watched machines get a row each; the rest are one level further in, under Connect to a
    // host…, because a `~/.ssh/config` with nine hosts in it is a list of possibilities and not a
    // list of anything happening.
    let (watched, offered): (Vec<&ConnectionMenu>, Vec<&ConnectionMenu>) = connections
        .iter()
        .filter(|it| !is_local(it))
        .partition(|it| is_watched(it));

    menu = menu.separator();
    for host in watched {
        menu = menu.item(&machine(app, host, &selection)?);
    }
    menu = menu.item(&hosts(app, &offered)?);

    // While following, the row names the session it picked. Nothing else in the menu can say so:
    // a tick on a session row means that session is pinned.
    let follow_text = match (&selection, &current) {
        (Selection::Auto, Some(entry)) => format!(
            "Follow most recent — {} · {}",
            entry.key.connection,
            cut(&entry.label, NAME_MAX)
        ),
        _ => "Follow most recent".to_owned(),
    };
    let follow = CheckMenuItemBuilder::with_id(FOLLOW, follow_text)
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

    // Asked of the window, not the config, so the row describes what the user can see. When she
    // is hidden this is the tray's row alone — there is no pet left to right-click.
    let shown = window::pet(app).is_none_or(|pet| window::is_shown(&pet));
    let (visibility, visibility_text) = if shown {
        (HIDE, "Hide Remi")
    } else {
        (SHOW, "Show Remi")
    };

    // ⚠️ The separator before Quit is a hit target, not decoration. Quit is unusually expensive
    // here — `LSUIElement` means no Dock icon and no Cmd-Tab entry, so quitting by accident ends
    // the app with nothing left on screen to say so — while its neighbour, Hide, costs one click
    // to undo. macOS gives a separator real height, so this buys the row above it a margin that
    // belongs to neither item.
    menu.separator()
        .item(&follow)
        .separator()
        .item(&sizes.build()?)
        .text(visibility, visibility_text)
        .separator()
        .text(QUIT, "Quit Remi")
        .build()
}

/// Whether the pet is listening to this machine — which is what decides where it is listed, since
/// a connection is [`ConnectionStatus::Disconnected`] exactly when nobody is watching it. A host
/// that is still connecting, or one whose source dropped and is retrying, counts: the user asked
/// for it, and what it is doing is news.
fn is_watched(connection: &ConnectionMenu) -> bool {
    !matches!(connection.status, ConnectionStatus::Disconnected)
}

/// Every machine the pet is not watching: the list to pick from when starting one, kept out of the
/// way of the machines that are actually reporting. Picking one connects to it, and it moves up to
/// a row of its own — first saying `connecting…`, then carrying its sessions.
///
/// The row is here even when there is nothing under it, because where the hosts come from is
/// otherwise invisible: a `~/.ssh/config` with nothing in it and one the pet never read look
/// identical from a menu that simply omits the row.
fn hosts(app: &AppHandle, offered: &[&ConnectionMenu]) -> tauri::Result<Submenu<Wry>> {
    let mut submenu = SubmenuBuilder::new(app, "Connect to a host…");
    if offered.is_empty() {
        let text = "No other hosts in ~/.ssh/config";
        return submenu
            .item(&MenuItemBuilder::new(text).enabled(false).build(app)?)
            .build();
    }
    for host in offered {
        // Nothing to say about it but its name: a machine nobody is watching has no sessions and
        // no state worth a word — that is what being in this list means.
        submenu = submenu.item(&connect_item(
            app,
            &host.connection,
            host.connection.as_str(),
        )?);
    }
    submenu.build()
}

/// The item that starts watching a machine. Its text differs by where it is: a name in the list of
/// hosts, the verb inside a machine's own submenu.
fn connect_item(app: &AppHandle, id: &ConnectionId, text: &str) -> tauri::Result<MenuItem<Wry>> {
    MenuItemBuilder::with_id(format!("{CONNECT}{id}"), text).build(app)
}

/// One machine's row: what it is doing, and what can be done about it.
///
/// Its sessions come first, because they are what the user opened the submenu for; the state of
/// the connection and what to do about it come after, where they are out of the way of a click
/// aimed at a session.
fn machine(
    app: &AppHandle,
    connection: &ConnectionMenu,
    selection: &Selection,
) -> tauri::Result<Submenu<Wry>> {
    let id = &connection.connection;
    let mut submenu = SubmenuBuilder::new(app, summary(connection));

    // A lost connection keeps its sessions, with the pose each was last seen in; a disconnected
    // one has none, because nobody is listening to that machine at all.
    let sessions = sessions(app, connection, selection, None)?;
    let had_sessions = !sessions.is_empty();
    for item in sessions {
        submenu = submenu.item(&item);
    }
    if had_sessions {
        submenu = submenu.separator();
    }

    submenu = match &connection.status {
        // A disconnected machine has no row of its own — it is offered under Connect to a host…
        // instead — so this is only reached in the moment between the user picking it there and
        // its source saying it has started.
        ConnectionStatus::Disconnected => submenu.item(&connect_item(app, id, "Connect")?),
        ConnectionStatus::Connecting => submenu
            .item(
                &MenuItemBuilder::new("Connecting…")
                    .enabled(false)
                    .build(app)?,
            )
            .item(&MenuItemBuilder::with_id(format!("{DISCONNECT}{id}"), "Cancel").build(app)?),
        ConnectionStatus::Lost { reason } => {
            // It is already retrying, so there is nothing here to press to make that happen; what
            // the user needs is the reason, and a way to stop.
            let text = format!("Reconnecting — {}", cut(reason, REASON_MAX));
            submenu
                .item(&MenuItemBuilder::new(text).enabled(false).build(app)?)
                .item(
                    &MenuItemBuilder::with_id(format!("{DISCONNECT}{id}"), "Disconnect")
                        .build(app)?,
                )
        }
        ConnectionStatus::Up => {
            if !had_sessions {
                submenu = submenu.item(
                    &MenuItemBuilder::new("No sessions")
                        .enabled(false)
                        .build(app)?,
                );
            }
            submenu.item(
                &MenuItemBuilder::with_id(format!("{DISCONNECT}{id}"), "Disconnect").build(app)?,
            )
        }
    };
    submenu.build()
}

/// A machine's row, as read without opening it.
///
/// The counts are facts about the machine rather than a pose chosen for it: the pet renders one
/// session and only one, so nothing here merges several sessions into a single state. What the
/// waiting count *is* for is the one thing this project exists to make noticeable — an agent
/// blocked on you, on a machine whose submenu is closed.
fn summary(connection: &ConnectionMenu) -> String {
    let name = &connection.connection;
    match &connection.status {
        ConnectionStatus::Disconnected => name.to_string(),
        ConnectionStatus::Connecting => format!("{name} — connecting…"),
        ConnectionStatus::Lost { reason } => {
            format!("{name} — disconnected: {}", cut(reason, REASON_MAX))
        }
        ConnectionStatus::Up => {
            let entries: Vec<&SessionEntry> = connection
                .harnesses
                .iter()
                .flat_map(|harness| &harness.sessions)
                .collect();
            let waiting = entries
                .iter()
                .filter(|entry| entry.pose == PetState::WaitingForInput)
                .count();
            match (entries.len(), waiting) {
                (0, _) => format!("{name} — no sessions"),
                (total, 0) => format!("{name} — {total} session{}", plural(total)),
                (total, waiting) => {
                    format!(
                        "{name} — {total} session{}, {waiting} waiting",
                        plural(total)
                    )
                }
            }
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// One checkable row per session on a connection, newest first.
///
/// `prefix` names the machine in each row, for the sessions listed flat; inside a machine's own
/// submenu it is left off, because the row it opened from already said which machine this is.
fn sessions(
    app: &AppHandle,
    connection: &ConnectionMenu,
    selection: &Selection,
    prefix: Option<&ConnectionId>,
) -> tauri::Result<Vec<CheckMenuItem<Wry>>> {
    let name_harness = connection.harnesses.len() > 1;
    let mut items = Vec::new();
    for harness in &connection.harnesses {
        for entry in &harness.sessions {
            let item = CheckMenuItemBuilder::with_id(
                session_id(&entry.key),
                row(prefix, entry, name_harness),
            )
            .checked(matches!(selection, Selection::Pinned(key) if *key == entry.key))
            .build(app)?;
            items.push(item);
        }
    }
    Ok(items)
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
        Action::Connect(connection) => connect(app, connection),
        Action::Disconnect(connection) => disconnect(app, connection),
        Action::Resize(named) => resize(app, named),
        Action::Visible(shown) => visible(app, shown),
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

/// Starts watching a machine, and writes it into the config so the next launch starts it again.
///
/// Which is the whole of "remembering a host": the config's connection list *is* the set of
/// things started at launch, so a fresh config watches this machine and nothing else, and every
/// host beyond that is there because the user asked for it once.
fn connect(app: &AppHandle, connection: ConnectionId) {
    let host = ssh_host(app, &connection);
    app.state::<Bridge>().connect(connection.clone(), host);
    save(app, |config| {
        if !config
            .connections
            .iter()
            .any(|it| it.name == connection.as_str())
        {
            config
                .connections
                .push(Connection::ssh(connection.as_str()));
        }
    });
}

/// Stops watching a machine and forgets it. The machine stays in the menu for as long as
/// `~/.ssh/config` names it — this removes the pet's interest in it, not the host.
fn disconnect(app: &AppHandle, connection: ConnectionId) {
    app.state::<Bridge>().disconnect(&connection);
    save(app, |config| {
        config
            .connections
            .retain(|it| it.name != connection.as_str())
    });
}

/// What to pass to `ssh` for this connection: whatever the config says, and otherwise the
/// connection's own name — which is the usual case, since a host is named after what the user
/// already types to reach it.
fn ssh_host(app: &AppHandle, connection: &ConnectionId) -> String {
    let config = app.state::<Arc<Mutex<Config>>>();
    let config = config.lock().expect("config mutex poisoned");
    config
        .connections
        .iter()
        .find(|it| it.name == connection.as_str())
        .map_or_else(|| connection.to_string(), |it| it.ssh_host().to_owned())
}

/// Resizes the window and remembers the new size. The renderer refits itself to whatever it is
/// given, so nothing has to be told about this.
/// Puts the pet away, or brings her back, and remembers which.
///
/// ⚠️ The tray is refreshed by hand here. Everything else in the menu is derived from the registry,
/// so the registry thread's own change check catches it (`bridge.rs`) — but visibility is the
/// user's, not the registry's, and nothing over there will ever notice it moved. Without this the
/// menu bar would go on offering Hide for a pet that is already hidden.
fn visible(app: &AppHandle, shown: bool) {
    let Some(pet) = window::pet(app) else {
        return;
    };
    window::show(&pet, shown);
    save(app, |config| config.window.hidden = !shown);
    crate::tray::refresh(app);
}

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
    if id == SHOW {
        return Some(Action::Visible(true));
    }
    if id == HIDE {
        return Some(Action::Visible(false));
    }
    // The whole of what follows is the connection's name, which is the user's to choose and so
    // the one part that may hold a `:`.
    if let Some(name) = id.strip_prefix(CONNECT) {
        return Some(Action::Connect(ConnectionId::new(name)));
    }
    if let Some(name) = id.strip_prefix(DISCONNECT) {
        return Some(Action::Disconnect(ConnectionId::new(name)));
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

/// One session's row: which machine it is on, what to call it, what it is doing, and — only when
/// it has stopped adding up — that it has gone quiet. See [`STALE_AFTER`].
fn row(connection: Option<&ConnectionId>, entry: &SessionEntry, name_harness: bool) -> String {
    let machine = connection.map_or_else(String::new, |connection| format!("{connection} · "));
    let harness = if name_harness {
        format!(" ({})", entry.key.harness)
    } else {
        String::new()
    };
    let quiet = if is_stale(entry.pose, entry.last_heard) {
        ", stale"
    } else {
        ""
    };
    format!(
        "{machine}{name}{harness} — {pose}{quiet}",
        name = cut(&entry.label, NAME_MAX),
        pose = pose(entry.pose),
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

/// How long a session that claims to be working may go unheard from before its row says so.
///
/// This stands in for the exact age rows used to carry ("writing, 2m"). The tray hands macOS a
/// finished menu and is never asked again (module docs), so a figure that moves every second would
/// be wrong for however long that menu sat there, while a marker that flips once and stays put is
/// as true an hour later as when it was built.
///
/// **Only the working poses are judged.** A session at `waiting`, `done` or `idle` is legitimately
/// silent for as long as the user leaves it alone — marking those would fire on the ordinary case
/// and teach the user to ignore it. One that claims to be thinking or writing should be producing
/// updates far more often than this, so past here the likelier story is that it died without ever
/// writing an end record. That case is the whole reason the age was in the row.
const STALE_AFTER: Duration = Duration::from_secs(120);

/// Whether a row should admit the session has gone quiet. See [`STALE_AFTER`].
fn is_stale(pose: PetState, last_heard: Duration) -> bool {
    let working = matches!(
        pose,
        PetState::Thinking | PetState::Viewing | PetState::Writing | PetState::Replying
    );
    working && last_heard >= STALE_AFTER
}

/// The snapshot as the *menu* reads it, with `last_heard` collapsed to the one bit of it a row can
/// show. Everything else in a snapshot changes only when something happens, so two of these
/// comparing equal means a freshly built menu would say exactly what the one already on screen
/// says — which is what lets the tray skip handing macOS a replacement it does not need.
///
/// Without this the raw `last_heard` would differ on every tick and the menu would be rebuilt once
/// a second forever, defeating the point of dropping the age from the row.
pub fn settled(snapshot: &MenuSnapshot) -> MenuSnapshot {
    let mut settled = snapshot.clone();
    let flatten = |entry: &mut SessionEntry| {
        entry.last_heard = if is_stale(entry.pose, entry.last_heard) {
            STALE_AFTER
        } else {
            Duration::ZERO
        };
    };
    for connection in &mut settled.connections {
        for harness in &mut connection.harnesses {
            harness.sessions.iter_mut().for_each(flatten);
        }
    }
    settled.current.iter_mut().for_each(flatten);
    settled
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

    fn host(name: &str, status: ConnectionStatus) -> ConnectionMenu {
        ConnectionMenu {
            connection: ConnectionId::new(name),
            status,
            harnesses: Vec::new(),
        }
    }

    #[test]
    fn a_machine_is_offered_to_connect_to_exactly_while_nobody_is_watching_it() {
        let hosts = [
            host("plume", ConnectionStatus::Up),
            host("theresa", ConnectionStatus::Connecting),
            host(
                "lappland",
                ConnectionStatus::Lost {
                    reason: "Permission denied (publickey)".into(),
                },
            ),
            host("whisperain", ConnectionStatus::Disconnected),
        ];
        let (watched, offered): (Vec<&ConnectionMenu>, Vec<&ConnectionMenu>) =
            hosts.iter().partition(|it| is_watched(it));

        // A host still connecting, and one whose source dropped and is retrying, are both the
        // user's doing and both have news — so they stay where the sessions are.
        let named = |hosts: Vec<&ConnectionMenu>| {
            hosts
                .iter()
                .map(|it| it.connection.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(named(watched), ["plume", "theresa", "lappland"]);
        assert_eq!(named(offered), ["whisperain"]);
    }

    #[test]
    fn connecting_and_disconnecting_name_the_machine() {
        // A connection name is the user's, so it may hold the separator.
        let Some(Action::Connect(connection)) = action("connect:home:1") else {
            panic!("connect:home:1 did not parse as connecting");
        };
        assert_eq!(connection, ConnectionId::new("home:1"));

        let Some(Action::Disconnect(connection)) = action("disconnect:theresa") else {
            panic!("disconnect:theresa did not parse as disconnecting");
        };
        assert_eq!(connection, ConnectionId::new("theresa"));
    }

    #[test]
    fn a_row_names_its_machine_only_where_the_menu_has_not_already() {
        let entry = SessionEntry {
            key: key("theresa", "claude-code", "s1"),
            label: "dotfiles".into(),
            pose: PetState::Writing,
            last_heard: Duration::from_secs(2),
            record: remi_core::record::SessionRecord::new(
                SessionId::new("s1").unwrap(),
                HarnessId::new("claude-code").unwrap(),
                PetState::Writing,
                0,
            ),
        };

        assert_eq!(
            row(Some(&ConnectionId::new("local")), &entry, false),
            "local · dotfiles — writing"
        );
        // Inside the machine's own submenu, the row it opened from already said which machine.
        assert_eq!(row(None, &entry, false), "dotfiles — writing");
    }

    #[test]
    fn a_row_admits_a_working_session_has_gone_quiet() {
        let mut entry = SessionEntry {
            key: key("theresa", "claude-code", "s1"),
            label: "dotfiles".into(),
            pose: PetState::Writing,
            last_heard: STALE_AFTER,
            record: remi_core::record::SessionRecord::new(
                SessionId::new("s1").unwrap(),
                HarnessId::new("claude-code").unwrap(),
                PetState::Writing,
                0,
            ),
        };
        assert_eq!(row(None, &entry, false), "dotfiles — writing, stale");

        // Waiting on the user is not going quiet, however long it lasts — marking it would fire on
        // the ordinary case rather than on the one worth noticing.
        entry.pose = PetState::WaitingForInput;
        entry.last_heard = Duration::from_secs(60 * 60 * 9);
        assert_eq!(row(None, &entry, false), "dotfiles — waiting");
    }

    #[test]
    fn the_fixed_items_parse_and_nothing_else_does() {
        assert!(matches!(action(FOLLOW), Some(Action::Follow)));
        assert!(matches!(action(QUIT), Some(Action::Quit)));
        // Which way round is in the id, so a click acts on what the row offered rather than on
        // whatever the pet happens to be doing by the time it lands.
        assert!(matches!(action(SHOW), Some(Action::Visible(true))));
        assert!(matches!(action(HIDE), Some(Action::Visible(false))));
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
    fn only_a_working_pose_can_go_stale() {
        assert!(is_stale(PetState::Writing, STALE_AFTER));
        assert!(is_stale(PetState::Thinking, STALE_AFTER * 30));
        // A minute into a long think is not yet worth doubting.
        assert!(!is_stale(PetState::Writing, STALE_AFTER / 2));
        // These are silent by their nature, so silence says nothing about them.
        assert!(!is_stale(PetState::WaitingForInput, STALE_AFTER * 30));
        assert!(!is_stale(PetState::Idle, STALE_AFTER * 30));
        assert!(!is_stale(PetState::Offline, STALE_AFTER * 30));
    }

    #[test]
    fn a_long_name_is_cut_on_a_character_boundary() {
        assert_eq!(cut("short", 10), "short");
        assert_eq!(cut("ありがとうございます", 4), "ありが…");
    }
}
