//! A state dir, watched for changes: the pet's `local` connection, and what `remi-hook watch`
//! runs on a remote, so the ssh connection hears exactly what a local one would.
//!
//! Every change is reported as the directory's whole listing, never as what changed: a session
//! that ended is simply missing from the next listing. Nothing is worked out from the file
//! events themselves, which each platform's watcher merges, reorders and names differently.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::record::SessionRecord;
use crate::store::{self, Store};

/// How long to keep collecting file events after the first one before listing the directory.
/// One hook write is a temp file plus a rename, and parallel tool calls fire several hooks at
/// once; waiting this long turns each burst into a single listing.
///
/// Write-to-delivery is around 29 ms end to end, most of which is the delay before the OS reports
/// the change at all; this constant is the smaller part.
///
/// ⚠️ It also bounds what can be observed. A record that changes and changes back inside one window
/// is coalesced away — the listing that follows is identical to the previous one, so nothing is
/// reported. A pose shorter than the notification delay therefore never arrives however this is
/// tuned, and polling would not help, since the record holds the value for no longer than that
/// either. [`crate::signal::Signal::ToolEnd`] is what produces such poses.
const SETTLE: Duration = Duration::from_millis(10);

/// A watch on one state dir that reports its whole listing each time it changes.
///
/// Synchronous: [`StateDirWatch::next_snapshot`] blocks, so a caller with other work to do
/// runs it on a thread of its own.
pub struct StateDirWatch {
    store: Store,
    /// Held only to keep the watch running; dropping it would stop the events.
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<notify::Event>>,
    /// The listing last returned, so an unchanged directory is not reported twice.
    last: Vec<SessionRecord>,
}

impl StateDirWatch {
    /// Starts watching `store`'s directory, creating it first if no session has written there
    /// yet, and returns the watch together with the current listing.
    pub fn start(store: Store) -> Result<(Self, Vec<SessionRecord>), Error> {
        store.create_dir()?;
        let watch_error = |source| Error::Watch {
            dir: store.dir().to_owned(),
            source,
        };
        let (sender, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(sender).map_err(watch_error)?;
        // Watching starts before the first listing, so a write that lands between the two
        // still produces an event rather than going unnoticed.
        watcher
            .watch(store.dir(), RecursiveMode::Recursive)
            .map_err(watch_error)?;
        let listing = store.list()?;

        let watch = Self {
            store,
            _watcher: watcher,
            events,
            last: listing.clone(),
        };
        Ok((watch, listing))
    }

    /// Blocks until the directory's listing differs from the one last returned, then returns
    /// the new listing. Changes that leave the listing as it was — a temp file appearing, or a
    /// record rewritten with the same content — are waited past.
    pub fn next_snapshot(&mut self) -> Result<Vec<SessionRecord>, Error> {
        loop {
            // Sleep until anything at all happens in the directory, then let the burst settle.
            let mut event = self.events.recv().map_err(|_| Error::Stopped)?;
            let deadline = Instant::now() + SETTLE;
            loop {
                // Which files an event names doesn't matter, since the directory is listed
                // afresh; an error from the watcher is still worth a line in the log.
                if let Err(err) = event {
                    tracing::warn!("file watcher error: {err}");
                }
                let left = deadline.saturating_duration_since(Instant::now());
                event = match self.events.recv_timeout(left) {
                    Ok(next) => next,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return Err(Error::Stopped),
                };
            }

            let listing = self.store.list()?;
            if listing != self.last {
                self.last = listing.clone();
                return Ok(listing);
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("watching {}: {source}", .dir.display())]
    Watch {
        dir: PathBuf,
        #[source]
        source: notify::Error,
    },
    #[error(transparent)]
    Store(#[from] store::Error),
    /// The watcher's own thread has gone away, so no further change will ever be seen.
    #[error("the file watcher stopped delivering events")]
    Stopped,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;

    use super::*;
    use crate::record::{HarnessId, SessionId};
    use crate::state::PetState;

    /// Far longer than any real wait, so a slow machine can't fail a test, while a watch that
    /// never reports still can't hang the suite.
    const PATIENCE: Duration = Duration::from_secs(5);

    fn record(harness: &str, session: &str, state: PetState) -> SessionRecord {
        SessionRecord::new(
            SessionId::new(session).unwrap(),
            HarnessId::new(harness).unwrap(),
            state,
            1_757_150_400,
        )
    }

    fn sessions(listing: &[SessionRecord]) -> Vec<&str> {
        listing
            .iter()
            .map(|record| record.session.as_str())
            .collect()
    }

    /// Runs `next_snapshot` on a thread, and fails the test if it hasn't returned within
    /// [`PATIENCE`].
    fn next_snapshot(mut watch: StateDirWatch) -> (StateDirWatch, Vec<SessionRecord>) {
        let (done, result) = mpsc::channel();
        thread::spawn(move || {
            let listing = watch.next_snapshot().unwrap();
            let _ = done.send((watch, listing));
        });
        result
            .recv_timeout(PATIENCE)
            .expect("the watch reported no change")
    }

    #[test]
    fn creates_a_missing_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("sessions"));

        let (_watch, listing) = StateDirWatch::start(store.clone()).unwrap();

        assert!(listing.is_empty());
        assert!(store.dir().is_dir());
    }

    #[test]
    fn starts_with_the_current_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        store
            .write(&record("claude-code", "s1", PetState::Thinking))
            .unwrap();

        let (_watch, listing) = StateDirWatch::start(store).unwrap();

        assert_eq!(sessions(&listing), ["s1"]);
    }

    #[test]
    fn reports_a_written_record() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        let (watch, _) = StateDirWatch::start(store.clone()).unwrap();

        store
            .write(&record("claude-code", "s1", PetState::Writing))
            .unwrap();

        let (_watch, listing) = next_snapshot(watch);
        assert_eq!(sessions(&listing), ["s1"]);
        assert_eq!(listing[0].state, PetState::Writing);
    }

    #[test]
    fn reports_an_ended_session_by_leaving_it_out() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        let ending = record("claude-code", "ending", PetState::Proud);
        store.write(&ending).unwrap();
        store
            .write(&record("claude-code", "staying", PetState::Thinking))
            .unwrap();
        let (watch, _) = StateDirWatch::start(store.clone()).unwrap();

        store.remove(&ending.harness, &ending.session).unwrap();

        let (_watch, listing) = next_snapshot(watch);
        assert_eq!(sessions(&listing), ["staying"]);
    }

    #[test]
    fn reports_a_session_under_a_harness_dir_created_after_the_watch_started() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        let (watch, _) = StateDirWatch::start(store.clone()).unwrap();

        store
            .write(&record("opencode", "ses_1", PetState::Thinking))
            .unwrap();

        let (_watch, listing) = next_snapshot(watch);
        assert_eq!(sessions(&listing), ["ses_1"]);
    }

    #[test]
    fn waits_past_changes_that_leave_the_listing_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        let unchanged = record("claude-code", "s1", PetState::Viewing);
        store.write(&unchanged).unwrap();
        let (watch, _) = StateDirWatch::start(store.clone()).unwrap();

        // Both of these touch the directory without changing what it lists.
        store.write(&unchanged).unwrap();
        fs::write(tmp.path().join("claude-code").join("s1.json.1.tmp"), b"{").unwrap();
        // The real change comes well after those have settled and been listed.
        let writer = store.clone();
        thread::spawn(move || {
            thread::sleep(SETTLE * 6);
            writer
                .write(&record("claude-code", "s2", PetState::Writing))
                .unwrap();
        });

        let (_watch, listing) = next_snapshot(watch);
        assert_eq!(sessions(&listing), ["s1", "s2"]);
    }
}
