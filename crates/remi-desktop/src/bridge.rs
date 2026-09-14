//! Registry → webview. One thread owns the [`Registry`]; sources, the menu and a clock all reach
//! it by channel, so nothing is ever locked and the registry needs no `Sync`.
//!
//! What reaches the webview is one `pet://state` event carrying the session Remi should show, and
//! it is emitted only when that changes. The 1 Hz tick exists because two of the registry's rules
//! are about time passing rather than about anything arriving: `Proud` fades to `Idle` on its own,
//! and an ended session leaves the menu on its own.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use remi_core::record::SessionRecord;
use remi_core::registry::{Registry, Selection, SessionEntry};
use remi_core::source::local::StateDirWatch;
use remi_core::source::{ConnectionId, SessionUpdate};
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

/// Anything that can change what Remi shows.
pub enum Message {
    Heard {
        connection: ConnectionId,
        update: SessionUpdate,
    },
    /// The user picked a session, or went back to following the newest.
    Select(Selection),
    Tick,
}

/// A handle for telling the registry thread something, and for asking what it last decided.
/// Cloneable, so the menu and every source can hold one.
#[derive(Clone)]
pub struct Bridge {
    tx: Sender<Message>,
    latest: Arc<Mutex<StatePayload>>,
}

impl Bridge {
    pub fn send(&self, message: Message) {
        // A closed channel means the registry thread is gone, which only happens on shutdown.
        let _ = self.tx.send(message);
    }

    /// The current pose, for a webview that has just finished loading.
    ///
    /// Tauri drops an event nobody is listening for yet, so a pet whose first snapshot arrives
    /// before `ui/app.js` subscribes would sit on `Idle` until the next thing the agent did. The
    /// last payload is kept here so a reader that attaches late can still see the current value.
    pub fn current(&self) -> StatePayload {
        self.latest.lock().expect("bridge mutex poisoned").clone()
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
                    Message::Heard { connection, update } => registry.apply(connection, update, now),
                    Message::Select(selection) => registry.select(selection),
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
    for connection in connections {
        match connection.kind {
            ConnectionKind::Local => start_local(bridge.clone(), ConnectionId::new(&connection.name)),
        }
    }
    bridge
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
