//! Registry → webview. One thread owns the [`Registry`]; sources, the menu and a clock all reach
//! it by channel, so nothing is ever locked and the registry needs no `Sync`.
//!
//! What reaches the webview is one `pet://state` event carrying the session Remi should show, and
//! it is emitted only when that changes. The 1 Hz tick exists because two of the registry's rules
//! are about time passing rather than about anything arriving: `Proud` fades to `Idle` on its own,
//! and an ended session leaves the menu on its own.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use remi_core::record::SessionRecord;
use remi_core::registry::{ConnectionMenu, Registry, Selection, SessionEntry};
use remi_core::source::local::StateDirWatch;
use remi_core::source::{ConnectionId, SessionUpdate, ssh, ssh_config};
use remi_core::state::PetState;
use remi_core::store::Store;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::config::{Connection, ConnectionKind};

/// Fast enough that `Proud`'s 8 s fade lands within a second of when it should, slow enough to be
/// free. It wakes a thread and compares two small values; it does not touch the disk.
const TICK: Duration = Duration::from_secs(1);

/// The Tauri event `ui/app.js` listens for.
const EVENT: &str = "pet://state";

/// How long [`Bridge::snapshot`] waits for the registry thread to answer. The thread never
/// blocks, so this only ever expires during shutdown; it is here so a right-click can never hang
/// the window instead of opening a menu.
const ANSWER_WITHIN: Duration = Duration::from_millis(500);

/// What the webview is told. `label` is resolved by the registry — title, else folder, else id —
/// so changing that rule never needs a new `remi-hook` on a remote.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StatePayload {
    pub state: PetState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl StatePayload {
    /// Nothing to show is not an error: it is a pet that has heard from no session yet, which
    /// looks exactly like every session having ended.
    fn of(entry: Option<&SessionEntry>) -> Self {
        match entry {
            None => Self {
                state: PetState::Offline,
                connection: None,
                session: None,
                label: None,
            },
            Some(entry) => Self {
                state: entry.pose,
                connection: Some(entry.key.connection.to_string()),
                session: Some(entry.key.session.to_string()),
                label: Some(entry.label.clone()),
            },
        }
    }
}

/// Everything the session menu shows, taken at one instant.
///
/// A menu is a still picture — its rows carry each session's pose and how long ago it was heard,
/// both of which move — so it is built from one snapshot rather than from several questions asked
/// a moment apart.
pub struct MenuSnapshot {
    pub connections: Vec<ConnectionMenu>,
    pub selection: Selection,
}

/// Anything that can change what Remi shows, plus the one question that only the registry thread
/// can answer.
pub enum Message {
    Heard {
        connection: ConnectionId,
        update: SessionUpdate,
    },
    /// The user picked a session, or went back to following the newest.
    Select(Selection),
    /// Connections the pet could use. They are listed in the menu before anything has been
    /// heard from them, which is how a host nobody has connected to yet can be offered at all.
    Declare(Vec<ConnectionId>),
    /// Someone is about to open the session menu and needs its contents.
    Menu(Sender<MenuSnapshot>),
    Tick,
}

/// A handle for telling the registry thread something, and for asking what it last decided.
/// Cloneable, so the menu and every source can hold one.
#[derive(Clone)]
pub struct Bridge {
    tx: Sender<Message>,
    latest: Arc<Mutex<StatePayload>>,
    /// The ssh connections currently being watched, so they can be stopped again. `local` is
    /// never in here: it is always on, and there is nothing to stop.
    ssh: Arc<Mutex<HashMap<ConnectionId, ssh::Stop>>>,
}

impl Bridge {
    pub fn send(&self, message: Message) {
        // A closed channel means the registry thread is gone, which only happens on shutdown.
        let _ = self.tx.send(message);
    }

    /// Everything the session menu needs. Asked of the registry thread rather than of a shared
    /// copy, so there is exactly one registry and it needs no lock.
    ///
    /// An unanswered request is not worth failing over: an empty menu still offers Follow most
    /// recent, Size and Quit, which is a better answer to a right-click than nothing happening.
    pub fn snapshot(&self) -> MenuSnapshot {
        let (tx, rx) = mpsc::channel();
        self.send(Message::Menu(tx));
        rx.recv_timeout(ANSWER_WITHIN).unwrap_or_else(|err| {
            tracing::error!("the registry did not answer the menu: {err}");
            MenuSnapshot {
                connections: Vec::new(),
                selection: Selection::Auto,
            }
        })
    }

    /// The current pose, for a webview that has just finished loading.
    ///
    /// Tauri drops an event nobody is listening for yet, so a pet whose first snapshot arrives
    /// before `ui/app.js` subscribes would sit on `Idle` until the next thing the agent did. The
    /// last payload is kept here so a reader that attaches late can still see the current value.
    pub fn current(&self) -> StatePayload {
        self.latest.lock().expect("bridge mutex poisoned").clone()
    }

    /// Starts watching a host over ssh, unless it is being watched already. Returns at once:
    /// connecting takes seconds, and the menu says so while it happens.
    pub fn connect(&self, connection: ConnectionId, host: String) {
        let stop = ssh::Stop::new();
        {
            let mut watching = self.ssh.lock().expect("bridge mutex poisoned");
            if watching.contains_key(&connection) {
                return tracing::debug!("already watching {connection}");
            }
            // The handle is made first and shared, rather than inserted and read back: reading it
            // back could find a disconnect had already taken it, and there is nothing sensible to
            // do about that but the thing this avoids having to do.
            watching.insert(connection.clone(), stop.clone());
        }

        tracing::info!("connecting to {connection} as ssh host {host}");
        let bridge = self.clone();
        let watched = connection.clone();
        let spawned = thread::Builder::new()
            .name(format!("remi-source-{connection}"))
            .spawn(move || {
                let connection = watched;
                ssh::watch(
                    || ssh::ssh_command(&host),
                    &stop,
                    |update| bridge.heard(&connection, update),
                );
                // ⚠️ Reported from *this* thread rather than from `disconnect`, so it cannot
                // overtake a snapshot this thread had already read when it was stopped — which
                // would put the connection back up, with sessions nobody is listening for.
                tracing::info!("stopped watching {connection}");
                bridge.heard(&connection, SessionUpdate::Disconnected);
            });
        if let Err(err) = spawned {
            self.ssh
                .lock()
                .expect("bridge mutex poisoned")
                .remove(&connection);
            let reason = format!("cannot start a thread for it: {err}");
            self.heard(&connection, SessionUpdate::ConnectionLost { reason });
        }
    }

    /// Stops watching a host. The connection keeps its place in the menu — it is still somewhere
    /// the user can connect to — but everything it was reporting is forgotten.
    pub fn disconnect(&self, connection: &ConnectionId) {
        match self.stop_taking(connection) {
            // The watch thread reports the disconnection itself, once it has actually ended.
            Some(stop) => stop.stop(),
            None => self.heard(connection, SessionUpdate::Disconnected),
        }
    }

    fn stop_taking(&self, connection: &ConnectionId) -> Option<ssh::Stop> {
        self.ssh
            .lock()
            .expect("bridge mutex poisoned")
            .remove(connection)
    }

    fn heard(&self, connection: &ConnectionId, update: SessionUpdate) {
        self.send(Message::Heard {
            connection: connection.clone(),
            update,
        });
    }
}

/// Starts the registry thread, the clock, and a source per configured connection.
pub fn start(app: AppHandle, connections: &[Connection], selection: Selection) -> Bridge {
    let (tx, rx) = mpsc::channel();
    let bridge = Bridge {
        tx,
        latest: Arc::new(Mutex::new(StatePayload::of(None))),
        ssh: Arc::new(Mutex::new(HashMap::new())),
    };

    let latest = bridge.latest.clone();
    thread::Builder::new()
        .name("remi-registry".into())
        .spawn(move || {
            let mut registry = Registry::default();
            registry.select(selection);
            // `None` rather than `Offline`, so the first real state is always emitted even when it
            // *is* Offline.
            let mut last: Option<StatePayload> = None;

            for message in rx {
                let now = Instant::now();
                match message {
                    Message::Heard { connection, update } => {
                        registry.apply(connection, update, now)
                    }
                    Message::Select(selection) => registry.select(selection),
                    Message::Declare(ids) => {
                        for id in ids {
                            registry.declare(id);
                        }
                    }
                    // Answering changes nothing, so it skips the emit below rather than
                    // recomputing a payload that cannot have moved.
                    Message::Menu(reply) => {
                        let _ = reply.send(MenuSnapshot {
                            connections: registry.menu(now),
                            selection: registry.selection().clone(),
                        });
                        continue;
                    }
                    Message::Tick => {}
                }

                let payload = StatePayload::of(registry.current(now).as_ref());
                if last.as_ref() != Some(&payload) {
                    tracing::debug!(?payload, "pet state changed");
                    // Published before it is emitted, so a webview that asks at exactly this
                    // moment cannot read a value older than the event it is about to receive.
                    *latest.lock().expect("bridge mutex poisoned") = payload.clone();
                    if let Err(err) = app.emit(EVENT, &payload) {
                        tracing::error!("emitting {EVENT}: {err}");
                    }
                    last = Some(payload);
                }
            }
        })
        .expect("spawning the registry thread");

    start_clock(bridge.clone());
    let offered = offered(connections);
    // One line at startup for what the menu will list, because "the host I expected is missing"
    // is otherwise a silent disagreement between this and the user's ~/.ssh/config.
    tracing::info!(
        "offering {}",
        offered
            .iter()
            .map(ConnectionId::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    bridge.send(Message::Declare(offered));

    for connection in connections {
        match connection.kind {
            ConnectionKind::Local => {
                start_local(bridge.clone(), ConnectionId::new(&connection.name))
            }
            // A configured ssh connection is one the user connected to before; the config is
            // what remembers it across a restart.
            ConnectionKind::Ssh => bridge.connect(
                ConnectionId::new(&connection.name),
                connection.ssh_host().to_owned(),
            ),
        }
    }
    bridge
}

/// Every connection the menu may offer: the configured ones, then every host the user's own
/// `~/.ssh/config` names. Nothing is connected to for being listed here — that is the point.
fn offered(connections: &[Connection]) -> Vec<ConnectionId> {
    let mut offered: Vec<ConnectionId> = connections
        .iter()
        .map(|connection| ConnectionId::new(&connection.name))
        .collect();
    for host in ssh_config::hosts() {
        let id = ConnectionId::new(host);
        if !offered.contains(&id) {
            offered.push(id);
        }
    }
    offered
}

fn start_clock(bridge: Bridge) {
    thread::Builder::new()
        .name("remi-tick".into())
        .spawn(move || {
            loop {
                thread::sleep(TICK);
                bridge.send(Message::Tick);
            }
        })
        .expect("spawning the clock thread");
}

/// Watches this machine's state dir. [`StateDirWatch`] blocks, hence a thread of its own.
///
/// Every change arrives as a whole listing rather than as a diff, which is what makes a session
/// that ended simply be missing — so this reports `Snapshot` and never has to work out removals.
fn start_local(bridge: Bridge, connection: ConnectionId) {
    thread::Builder::new()
        .name("remi-source-local".into())
        .spawn(move || {
            let store = match Store::locate() {
                Ok(store) => store,
                Err(err) => return lost(&bridge, &connection, err.to_string()),
            };
            tracing::info!("watching {}", store.dir().display());

            let (mut watch, listing) = match StateDirWatch::start(store) {
                Ok(started) => started,
                Err(err) => return lost(&bridge, &connection, err.to_string()),
            };
            snapshot(&bridge, &connection, listing);

            loop {
                match watch.next_snapshot() {
                    Ok(listing) => snapshot(&bridge, &connection, listing),
                    Err(err) => return lost(&bridge, &connection, err.to_string()),
                }
            }
        })
        .expect("spawning the local source thread");
}

fn snapshot(bridge: &Bridge, connection: &ConnectionId, listing: Vec<SessionRecord>) {
    bridge.heard(connection, SessionUpdate::Snapshot(listing));
}

/// The sessions this connection already reported keep their last pose — the registry holds them —
/// so a source dying is visible in the menu rather than blanking the pet.
fn lost(bridge: &Bridge, connection: &ConnectionId, reason: String) {
    tracing::error!("connection {connection} lost: {reason}");
    bridge.heard(connection, SessionUpdate::ConnectionLost { reason });
}
