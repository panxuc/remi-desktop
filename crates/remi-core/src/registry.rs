//! Everything the pet knows about sessions, across all connections, and the two questions the
//! UI asks of it: what goes in the session menu, and what Remi should show right now.
//!
//! Pure: no I/O and no clock of its own. Updates and the current time are passed in, so every
//! rule here can be tested by stepping a made-up clock.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::time::{Duration, Instant};

use crate::record::{HarnessId, SessionId, SessionRecord};
use crate::source::{ConnectionId, SessionUpdate};
use crate::state::PetState;

/// How long `Proud` shows after a turn ends before it fades to `Idle`.
///
/// No other pose ever times out. Hooks write only when something happens, so a session
/// blocked on an approval prompt, or running a long tool, is silent for as long as that lasts;
/// a timeout would hide exactly the pose the pet exists to show. A session that dies without
/// ending keeps its last pose, and the menu's "last heard" is what gives it away.
pub const PROUD_FADES_AFTER: Duration = Duration::from_secs(8);

/// How long a session that has ended stays in the menu, shown as `Offline`, before it is
/// dropped: long enough to notice that it ended. A pinned session stays for as long as it is
/// pinned.
pub const ENDED_SHOWN_FOR: Duration = Duration::from_secs(10);

/// One session among everything the pet hears. The same session id can exist under two
/// harnesses, and one machine can be reached through two connections, so all three parts are
/// needed. Ordered by connection, then harness, then session: the order the menu groups them.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey {
    pub connection: ConnectionId,
    pub harness: HarnessId,
    pub session: SessionId,
}

impl SessionKey {
    fn of(connection: &ConnectionId, record: &SessionRecord) -> Self {
        Self {
            connection: connection.clone(),
            harness: record.harness.clone(),
            session: record.session.clone(),
        }
    }
}

/// Which session the user wants Remi to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    /// Whichever session has the newest record. When that session ends, it shows as `Offline`
    /// for [`ENDED_SHOWN_FOR`], and then the next newest takes over.
    #[default]
    Auto,
    /// This session, even after it ends: it then stays, shown as `Offline`, until the user
    /// selects something else. While the pet has never heard of it — say, a pin restored from
    /// config for a session that ended while the pet was closed — Remi follows Auto.
    Pinned(SessionKey),
}

/// Whether a connection is currently delivering updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Up,
    /// Its source is retrying. Sessions heard before stay listed with their last pose.
    Lost {
        reason: String,
    },
}

/// One session as the UI shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    pub key: SessionKey,
    /// The session's name in the menu: its title, else its folder, else its id. Chosen here,
    /// not by the writer, so changing the rule never needs a new `remi-hook` on the remotes.
    pub label: String,
    /// What Remi shows for the session now: `Offline` once the session has ended, otherwise
    /// its record's state, with `Proud` already faded to `Idle` once it has run out.
    pub pose: PetState,
    /// How long ago the session's current record arrived.
    pub last_heard: Duration,
    pub record: SessionRecord,
}

/// One connection's part of the session menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionMenu {
    pub connection: ConnectionId,
    pub status: ConnectionStatus,
    /// Harnesses by id. Empty when the connection has no sessions to show.
    pub harnesses: Vec<HarnessMenu>,
}

/// The sessions of one harness on one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessMenu {
    pub harness: HarnessId,
    /// Newest record first.
    pub sessions: Vec<SessionEntry>,
}

#[derive(Debug, Default)]
pub struct Registry {
    sessions: BTreeMap<SessionKey, StoredSession>,
    connections: BTreeMap<ConnectionId, ConnectionStatus>,
    /// Kept here rather than by the UI, because whether an ended session may be dropped
    /// depends on whether it is pinned.
    selection: Selection,
}

impl Registry {
    /// Takes in an update from `connection`, heard at `now`.
    pub fn apply(&mut self, connection: ConnectionId, update: SessionUpdate, now: Instant) {
        let status = match &update {
            SessionUpdate::ConnectionLost { reason } => ConnectionStatus::Lost {
                reason: reason.clone(),
            },
            // Anything else a connection delivers shows it is working.
            _ => ConnectionStatus::Up,
        };
        self.connections.insert(connection.clone(), status);

        match update {
            SessionUpdate::Upsert(record) => {
                let key = SessionKey::of(&connection, &record);
                let previous = self.sessions.get(&key);
                // Ties are taken: `ts` is in whole seconds, so consecutive events often share
                // one, and the later arrival is the later event.
                if previous.is_none_or(|stored| record.ts >= stored.record.ts) {
                    let stored = StoredSession::new(record, previous, now);
                    self.sessions.insert(key, stored);
                }
            }
            SessionUpdate::Removed { harness, session } => {
                let key = SessionKey {
                    connection,
                    harness,
                    session,
                };
                if let Some(stored) = self.sessions.get_mut(&key) {
                    stored.ended.get_or_insert(now);
                }
            }
            SessionUpdate::Snapshot(records) => {
                // A snapshot is the connection's whole truth: no record in it is refused for
                // being older, and every session it leaves out has ended.
                let (before, others) = std::mem::take(&mut self.sessions)
                    .into_iter()
                    .partition::<BTreeMap<_, _>, _>(|(key, _)| key.connection == connection);
                self.sessions = others;
                for record in records {
                    let key = SessionKey::of(&connection, &record);
                    let stored = StoredSession::new(record, before.get(&key), now);
                    self.sessions.insert(key, stored);
                }
                for (key, mut stored) in before {
                    if let Entry::Vacant(slot) = self.sessions.entry(key) {
                        // One that had already ended keeps its original end time.
                        stored.ended.get_or_insert(now);
                        slot.insert(stored);
                    }
                }
            }
            SessionUpdate::ConnectionUp | SessionUpdate::ConnectionLost { .. } => {}
        }

        // Ended sessions that nothing shows any more are dropped, so they don't pile up in a
        // pet left running for weeks.
        let selection = &self.selection;
        self.sessions
            .retain(|key, stored| stored.is_shown(key, selection, now));
    }

    /// Changes which session Remi shows. An ended session that is no longer pinned disappears
    /// once its [`ENDED_SHOWN_FOR`] has run out.
    pub fn select(&mut self, selection: Selection) {
        self.selection = selection;
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The session menu: connections by name, each one's harnesses by id, and each harness's
    /// sessions newest first. Names rather than activity order the connections and harnesses,
    /// so they don't move around between rebuilds. A connection with no sessions is still
    /// listed, so a host that is down or idle stays visible. Ended sessions are listed, as
    /// `Offline`, for [`ENDED_SHOWN_FOR`], or for as long as they are pinned.
    pub fn menu(&self, now: Instant) -> Vec<ConnectionMenu> {
        let mut menu = Vec::new();
        for (connection, status) in &self.connections {
            let mut harnesses: Vec<HarnessMenu> = Vec::new();
            // Sessions are in key order, so each harness's sessions come one after another.
            for (key, stored) in &self.sessions {
                if key.connection != *connection || !stored.is_shown(key, &self.selection, now) {
                    continue;
                }
                let entry = stored.entry(key, now);
                match harnesses.last_mut() {
                    Some(group) if group.harness == key.harness => group.sessions.push(entry),
                    _ => harnesses.push(HarnessMenu {
                        harness: key.harness.clone(),
                        sessions: vec![entry],
                    }),
                }
            }
            for group in &mut harnesses {
                group.sessions.sort_by_key(|entry| Reverse(entry.record.ts));
            }
            menu.push(ConnectionMenu {
                connection: connection.clone(),
                status: status.clone(),
                harnesses,
            });
        }
        menu
    }

    /// The session Remi should show, or `None` when there is none, in which case Remi shows
    /// `Offline`.
    ///
    /// Auto takes the newest record by its writer's clock rather than by when the pet heard it,
    /// because a source attaching delivers all of its sessions at the same moment. Ties go to
    /// the last session in key order.
    pub fn current(&self, now: Instant) -> Option<SessionEntry> {
        let pinned = match &self.selection {
            Selection::Pinned(key) => self.sessions.get_key_value(key),
            Selection::Auto => None,
        };
        let (key, stored) = pinned.or_else(|| {
            self.sessions
                .iter()
                .filter(|(key, stored)| stored.is_shown(key, &self.selection, now))
                .max_by_key(|(_, stored)| stored.record.ts)
        })?;
        Some(stored.entry(key, now))
    }
}

/// A session's current record, when the pet first heard it, and whether it has ended.
#[derive(Clone, Debug)]
struct StoredSession {
    record: SessionRecord,
    /// On the pet's own clock, never the writer's, so clock skew between machines can neither
    /// cut `Proud` short nor make a live session look hours old.
    received: Instant,
    /// When the pet learned the session had ended, on its own clock. `None` while it is live.
    ended: Option<Instant>,
}

impl StoredSession {
    /// `record`, heard at `now`, replacing `previous`. A session heard from is live, even if it
    /// had ended. Hearing the same record again keeps the original arrival time: a source
    /// re-attaching resends every record, and that must not restart `Proud` or reset "last
    /// heard". Any real write changes at least `ts`.
    fn new(record: SessionRecord, previous: Option<&StoredSession>, now: Instant) -> Self {
        let received = previous
            .filter(|stored| stored.record == record)
            .map_or(now, |stored| stored.received);
        Self {
            record,
            received,
            ended: None,
        }
    }

    /// Whether the menu and Auto still show this session: always while it is live, for
    /// [`ENDED_SHOWN_FOR`] after it ends, and for as long as it is pinned.
    fn is_shown(&self, key: &SessionKey, selection: &Selection, now: Instant) -> bool {
        match self.ended {
            None => true,
            Some(ended) => {
                let pinned = matches!(selection, Selection::Pinned(pin) if pin == key);
                pinned || now.saturating_duration_since(ended) < ENDED_SHOWN_FOR
            }
        }
    }

    fn entry(&self, key: &SessionKey, now: Instant) -> SessionEntry {
        let last_heard = now.saturating_duration_since(self.received);
        let pose = match self.record.state {
            _ if self.ended.is_some() => PetState::Offline,
            PetState::Proud if last_heard >= PROUD_FADES_AFTER => PetState::Idle,
            state => state,
        };
        let label = self
            .record
            .title
            .as_deref()
            .filter(|title| !title.is_empty())
            .or(self.record.cwd.as_deref().filter(|cwd| !cwd.is_empty()))
            .unwrap_or(self.record.session.as_str())
            .to_owned();
        SessionEntry {
            key: key.clone(),
            label,
            pose,
            last_heard,
            record: self.record.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PetState::*;

    fn record(harness: &str, session: &str, state: PetState, ts: i64) -> SessionRecord {
        SessionRecord::new(
            SessionId::new(session).unwrap(),
            HarnessId::new(harness).unwrap(),
            state,
            ts,
        )
    }

    fn key(connection: &str, harness: &str, session: &str) -> SessionKey {
        SessionKey {
            connection: ConnectionId::new(connection),
            harness: HarnessId::new(harness).unwrap(),
            session: SessionId::new(session).unwrap(),
        }
    }

    fn upsert(registry: &mut Registry, connection: &str, record: SessionRecord, now: Instant) {
        registry.apply(
            ConnectionId::new(connection),
            SessionUpdate::Upsert(record),
            now,
        );
    }

    fn snapshot(
        registry: &mut Registry,
        connection: &str,
        records: Vec<SessionRecord>,
        now: Instant,
    ) {
        registry.apply(
            ConnectionId::new(connection),
            SessionUpdate::Snapshot(records),
            now,
        );
    }

    fn remove(
        registry: &mut Registry,
        connection: &str,
        harness: &str,
        session: &str,
        now: Instant,
    ) {
        registry.apply(
            ConnectionId::new(connection),
            SessionUpdate::Removed {
                harness: HarnessId::new(harness).unwrap(),
                session: SessionId::new(session).unwrap(),
            },
            now,
        );
    }

    /// Which session Remi shows, and in what pose.
    fn shown(registry: &Registry, now: Instant) -> Option<(SessionKey, PetState)> {
        registry.current(now).map(|entry| (entry.key, entry.pose))
    }

    /// The menu reduced to names: `(connection, [(harness, [session])])`.
    type MenuShape = Vec<(String, Vec<(String, Vec<String>)>)>;

    /// The menu's shape, for comparing with an expected one.
    fn menu_shape(registry: &Registry, now: Instant) -> MenuShape {
        registry
            .menu(now)
            .into_iter()
            .map(|connection| {
                let harnesses = connection
                    .harnesses
                    .into_iter()
                    .map(|group| {
                        let sessions = group
                            .sessions
                            .into_iter()
                            .map(|entry| entry.key.session.to_string())
                            .collect();
                        (group.harness.to_string(), sessions)
                    })
                    .collect();
                (connection.connection.to_string(), harnesses)
            })
            .collect()
    }

    #[test]
    fn with_no_sessions_nothing_is_shown() {
        let registry = Registry::default();
        let now = Instant::now();
        assert_eq!(registry.current(now), None);
        assert!(registry.menu(now).is_empty());
    }

    #[test]
    fn proud_fades_to_idle() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Proud, 100),
            t0,
        );

        let just_before = t0 + PROUD_FADES_AFTER - Duration::from_millis(1);
        assert_eq!(shown(&registry, just_before).unwrap().1, Proud);
        assert_eq!(shown(&registry, t0 + PROUD_FADES_AFTER).unwrap().1, Idle);
    }

    #[test]
    fn no_other_pose_times_out() {
        let t0 = Instant::now();
        let a_day_later = t0 + Duration::from_secs(24 * 60 * 60);
        for state in [Thinking, Viewing, Writing, WaitingForInput, Offline] {
            let mut registry = Registry::default();
            upsert(
                &mut registry,
                "local",
                record("claude-code", "s1", state, 100),
                t0,
            );
            assert_eq!(shown(&registry, a_day_later).unwrap().1, state);
        }
    }

    #[test]
    fn an_older_record_is_ignored_and_a_tie_is_taken() {
        let mut registry = Registry::default();
        let now = Instant::now();

        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Writing, 100),
            now,
        );
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Thinking, 99),
            now,
        );
        assert_eq!(shown(&registry, now).unwrap().1, Writing);

        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Viewing, 100),
            now,
        );
        assert_eq!(shown(&registry, now).unwrap().1, Viewing);
    }

    #[test]
    fn hearing_the_same_record_again_keeps_its_arrival_time() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        let proud = record("claude-code", "s1", Proud, 100);
        let later = t0 + Duration::from_secs(5);

        upsert(&mut registry, "local", proud.clone(), t0);
        upsert(&mut registry, "local", proud.clone(), later);
        snapshot(&mut registry, "local", vec![proud], later);

        let entry = registry.current(t0 + PROUD_FADES_AFTER).unwrap();
        assert_eq!(entry.pose, Idle, "Proud was not restarted");
        assert_eq!(entry.last_heard, PROUD_FADES_AFTER);
    }

    #[test]
    fn a_changed_record_restarts_its_arrival_time() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(10);

        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Proud, 100),
            t0,
        );
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Proud, 110),
            later,
        );

        let entry = registry.current(later + Duration::from_secs(1)).unwrap();
        assert_eq!(entry.pose, Proud);
        assert_eq!(entry.last_heard, Duration::from_secs(1));
    }

    #[test]
    fn a_session_left_out_of_a_snapshot_shows_offline_then_is_dropped() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Writing, 100),
            t0,
        );

        snapshot(&mut registry, "local", vec![], t0);

        let just_before = t0 + ENDED_SHOWN_FOR - Duration::from_millis(1);
        assert_eq!(
            shown(&registry, just_before),
            Some((key("local", "claude-code", "s1"), Offline))
        );
        assert_eq!(
            menu_shape(&registry, just_before),
            [(
                "local".into(),
                vec![("claude-code".into(), vec!["s1".into()])]
            )]
        );

        let after = t0 + ENDED_SHOWN_FOR;
        assert_eq!(shown(&registry, after), None);
        assert_eq!(menu_shape(&registry, after), [("local".into(), vec![])]);

        registry.apply(
            ConnectionId::new("local"),
            SessionUpdate::ConnectionUp,
            after,
        );
        assert!(registry.sessions.is_empty(), "dropped from storage too");
    }

    #[test]
    fn a_removed_session_ends_only_under_its_harness() {
        let mut registry = Registry::default();
        let now = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Writing, 100),
            now,
        );
        upsert(
            &mut registry,
            "local",
            record("opencode", "s1", Writing, 100),
            now,
        );

        remove(&mut registry, "local", "claude-code", "s1", now);

        let poses: Vec<(String, PetState)> = registry.menu(now)[0]
            .harnesses
            .iter()
            .map(|group| (group.harness.to_string(), group.sessions[0].pose))
            .collect();
        assert_eq!(
            poses,
            [
                ("claude-code".to_owned(), Offline),
                ("opencode".to_owned(), Writing)
            ]
        );
        assert_eq!(
            menu_shape(&registry, now + ENDED_SHOWN_FOR),
            [("local".into(), vec![("opencode".into(), vec!["s1".into()])])]
        );
    }

    #[test]
    fn a_later_snapshot_does_not_restart_the_ended_time() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Writing, 100),
            t0,
        );

        snapshot(&mut registry, "local", vec![], t0);
        snapshot(&mut registry, "local", vec![], t0 + Duration::from_secs(5));

        assert_eq!(shown(&registry, t0 + ENDED_SHOWN_FOR), None);
    }

    #[test]
    fn an_ended_session_heard_from_again_is_live_again() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Writing, 100),
            t0,
        );
        snapshot(&mut registry, "local", vec![], t0);

        let later = t0 + Duration::from_secs(1);
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Thinking, 110),
            later,
        );

        assert_eq!(
            shown(&registry, later + ENDED_SHOWN_FOR),
            Some((key("local", "claude-code", "s1"), Thinking))
        );
    }

    #[test]
    fn a_snapshot_replaces_only_its_own_connection() {
        let mut registry = Registry::default();
        let now = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s1", Writing, 100),
            now,
        );
        upsert(
            &mut registry,
            "local",
            record("claude-code", "s2", Writing, 200),
            now,
        );
        upsert(
            &mut registry,
            "plume",
            record("claude-code", "s3", Writing, 300),
            now,
        );

        snapshot(
            &mut registry,
            "local",
            vec![
                // Older than the stored record, but a snapshot is taken as it is.
                record("claude-code", "s2", Thinking, 150),
                record("claude-code", "s4", Thinking, 50),
            ],
            now,
        );

        let later = now + ENDED_SHOWN_FOR;
        assert_eq!(
            menu_shape(&registry, later),
            [
                (
                    "local".into(),
                    vec![("claude-code".into(), vec!["s2".into(), "s4".into()])]
                ),
                (
                    "plume".into(),
                    vec![("claude-code".into(), vec!["s3".into()])]
                ),
            ]
        );
        assert_eq!(
            registry.menu(later)[0].harnesses[0].sessions[0].pose,
            Thinking
        );
    }

    #[test]
    fn a_lost_connection_keeps_its_sessions_until_it_delivers_again() {
        let mut registry = Registry::default();
        let now = Instant::now();
        upsert(
            &mut registry,
            "plume",
            record("claude-code", "s1", WaitingForInput, 100),
            now,
        );

        registry.apply(
            ConnectionId::new("plume"),
            SessionUpdate::ConnectionLost {
                reason: "connection reset".into(),
            },
            now,
        );
        let menu = registry.menu(now);
        assert_eq!(
            menu[0].status,
            ConnectionStatus::Lost {
                reason: "connection reset".into()
            }
        );
        assert_eq!(menu[0].harnesses[0].sessions[0].pose, WaitingForInput);
        assert_eq!(
            shown(&registry, now),
            Some((key("plume", "claude-code", "s1"), WaitingForInput))
        );

        upsert(
            &mut registry,
            "plume",
            record("claude-code", "s1", Thinking, 101),
            now,
        );
        assert_eq!(registry.menu(now)[0].status, ConnectionStatus::Up);
    }

    #[test]
    fn a_connection_with_no_sessions_is_still_listed() {
        let mut registry = Registry::default();
        let now = Instant::now();
        registry.apply(ConnectionId::new("plume"), SessionUpdate::ConnectionUp, now);

        assert_eq!(menu_shape(&registry, now), [("plume".into(), vec![])]);
        assert_eq!(registry.current(now), None);
    }

    #[test]
    fn auto_shows_the_newest_record_across_connections() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        // Heard in the opposite order to their timestamps.
        upsert(
            &mut registry,
            "plume",
            record("claude-code", "new", Writing, 200),
            t0,
        );
        let later = t0 + Duration::from_secs(1);
        upsert(
            &mut registry,
            "local",
            record("claude-code", "old", Thinking, 100),
            later,
        );

        assert_eq!(
            shown(&registry, later),
            Some((key("plume", "claude-code", "new"), Writing))
        );
    }

    #[test]
    fn auto_breaks_a_tie_by_taking_the_last_key() {
        let mut registry = Registry::default();
        let now = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "a", Writing, 100),
            now,
        );
        upsert(
            &mut registry,
            "local",
            record("opencode", "b", Thinking, 100),
            now,
        );

        assert_eq!(
            shown(&registry, now),
            Some((key("local", "opencode", "b"), Thinking))
        );
    }

    #[test]
    fn auto_moves_on_from_an_ended_session_after_a_while() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "older", Thinking, 100),
            t0,
        );
        upsert(
            &mut registry,
            "local",
            record("claude-code", "newest", Writing, 200),
            t0,
        );

        remove(&mut registry, "local", "claude-code", "newest", t0);

        assert_eq!(
            shown(&registry, t0 + Duration::from_secs(1)),
            Some((key("local", "claude-code", "newest"), Offline))
        );
        assert_eq!(
            shown(&registry, t0 + ENDED_SHOWN_FOR),
            Some((key("local", "claude-code", "older"), Thinking))
        );
    }

    #[test]
    fn a_pinned_session_is_shown_even_when_older() {
        let mut registry = Registry::default();
        let now = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "old", Thinking, 100),
            now,
        );
        upsert(
            &mut registry,
            "plume",
            record("claude-code", "new", Writing, 200),
            now,
        );

        registry.select(Selection::Pinned(key("local", "claude-code", "old")));

        assert_eq!(
            shown(&registry, now),
            Some((key("local", "claude-code", "old"), Thinking))
        );
    }

    #[test]
    fn a_pinned_session_stays_after_it_ends_until_something_else_is_selected() {
        let mut registry = Registry::default();
        let t0 = Instant::now();
        let other = record("claude-code", "other", Thinking, 200);
        upsert(
            &mut registry,
            "local",
            record("claude-code", "pinned", Writing, 100),
            t0,
        );
        upsert(&mut registry, "local", other.clone(), t0);
        registry.select(Selection::Pinned(key("local", "claude-code", "pinned")));

        snapshot(&mut registry, "local", vec![other], t0);
        // Updates keep arriving long after it ended, and none of them drops it.
        let a_day_later = t0 + Duration::from_secs(24 * 60 * 60);
        upsert(
            &mut registry,
            "local",
            record("claude-code", "other", Viewing, 300),
            a_day_later,
        );

        assert_eq!(
            shown(&registry, a_day_later),
            Some((key("local", "claude-code", "pinned"), Offline))
        );
        assert_eq!(
            menu_shape(&registry, a_day_later),
            [(
                "local".into(),
                vec![("claude-code".into(), vec!["other".into(), "pinned".into()])]
            )]
        );

        registry.select(Selection::Auto);
        assert_eq!(
            shown(&registry, a_day_later),
            Some((key("local", "claude-code", "other"), Viewing))
        );
        assert_eq!(
            menu_shape(&registry, a_day_later),
            [(
                "local".into(),
                vec![("claude-code".into(), vec!["other".into()])]
            )]
        );
        registry.apply(
            ConnectionId::new("local"),
            SessionUpdate::ConnectionUp,
            a_day_later,
        );
        assert_eq!(
            registry.sessions.len(),
            1,
            "dropped from storage once unpinned"
        );
    }

    #[test]
    fn a_pin_the_pet_has_not_heard_of_follows_auto_until_it_appears() {
        let mut registry = Registry::default();
        let now = Instant::now();
        upsert(
            &mut registry,
            "local",
            record("claude-code", "other", Writing, 200),
            now,
        );
        registry.select(Selection::Pinned(key("local", "claude-code", "pinned")));

        assert_eq!(
            shown(&registry, now),
            Some((key("local", "claude-code", "other"), Writing))
        );

        upsert(
            &mut registry,
            "local",
            record("claude-code", "pinned", Viewing, 100),
            now,
        );
        assert_eq!(
            shown(&registry, now),
            Some((key("local", "claude-code", "pinned"), Viewing))
        );
    }

    #[test]
    fn the_same_session_through_two_connections_is_two_entries() {
        let mut registry = Registry::default();
        let now = Instant::now();
        let same = record("claude-code", "s1", Writing, 100);
        upsert(&mut registry, "plume", same.clone(), now);
        upsert(&mut registry, "plume-mqtt", same, now);

        assert_eq!(
            menu_shape(&registry, now),
            [
                (
                    "plume".into(),
                    vec![("claude-code".into(), vec!["s1".into()])]
                ),
                (
                    "plume-mqtt".into(),
                    vec![("claude-code".into(), vec!["s1".into()])]
                ),
            ]
        );
    }

    #[test]
    fn labels_prefer_the_title_then_the_folder_then_the_session_id() {
        let cases = [
            // (title, cwd, expected label)
            (Some("Creating bin"), Some("remi-desktop"), "Creating bin"),
            (None, Some("remi-desktop"), "remi-desktop"),
            (Some(""), Some("remi-desktop"), "remi-desktop"),
            (None, None, "s1"),
            (Some(""), Some(""), "s1"),
        ];
        let now = Instant::now();
        for (title, cwd, expected) in cases {
            let mut named = record("claude-code", "s1", Thinking, 100);
            named.title = title.map(str::to_owned);
            named.cwd = cwd.map(str::to_owned);
            let mut registry = Registry::default();
            upsert(&mut registry, "local", named, now);

            let label = registry.current(now).unwrap().label;
            assert_eq!(label, expected, "title {title:?}, cwd {cwd:?}");
        }
    }

    #[test]
    fn the_menu_groups_by_connection_then_harness_with_the_newest_sessions_first() {
        let mut registry = Registry::default();
        let now = Instant::now();
        upsert(
            &mut registry,
            "plume",
            record("claude-code", "d", Writing, 50),
            now,
        );
        upsert(
            &mut registry,
            "local",
            record("opencode", "c", Writing, 200),
            now,
        );
        upsert(
            &mut registry,
            "local",
            record("claude-code", "a", Writing, 100),
            now,
        );
        upsert(
            &mut registry,
            "local",
            record("claude-code", "b", Writing, 300),
            now,
        );

        assert_eq!(
            menu_shape(&registry, now),
            [
                (
                    "local".into(),
                    vec![
                        ("claude-code".into(), vec!["b".into(), "a".into()]),
                        ("opencode".into(), vec!["c".into()]),
                    ]
                ),
                (
                    "plume".into(),
                    vec![("claude-code".into(), vec!["d".into()])]
                ),
            ]
        );
    }
}
