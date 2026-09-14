use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::record::{self, HarnessId, SessionId, SessionRecord};

/// Overrides the state directory. Meant for tests.
pub const STATE_DIR_ENV: &str = "REMI_STATE_DIR";

/// How long a session file may go unwritten before [`Store::prune`] deletes it: long enough
/// that no live session is ever affected, short enough that crashed ones don't pile up.
pub const PRUNE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The directory of session state files on one machine: one directory per harness, named by
/// its id, holding one `<session id>.json` per session with that session's current
/// [`SessionRecord`]. Sorting by harness keeps two harnesses' sessions apart and makes the
/// directory easy to read when debugging.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// `$REMI_STATE_DIR` if set, else `<home>/.local/state/remi/sessions` on every OS.
    ///
    /// Only the home directory is consulted — never `$XDG_STATE_HOME` or anything else a shell
    /// rc might set — because a hook launched from an interactive shell and a `watch` launched
    /// by a non-interactive ssh must resolve the same directory.
    pub fn locate() -> Result<Self, Error> {
        if let Some(dir) = std::env::var_os(STATE_DIR_ENV).filter(|dir| !dir.is_empty()) {
            return Ok(Self::at(dir));
        }
        let base = directories::BaseDirs::new().ok_or(Error::NoHome)?;
        Ok(Self::at(
            base.home_dir()
                .join(".local")
                .join("state")
                .join("remi")
                .join("sessions"),
        ))
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Replaces the session's file atomically: the record goes to a temp file beside it, which
    /// is then renamed over the old one, so a reader never sees a half-written record.
    ///
    /// No fsync: a record lost to a power cut is replaced by the session's next event, and a
    /// hook has only milliseconds to spend.
    pub fn write(&self, record: &SessionRecord) -> Result<(), Error> {
        let dir = self.dir.join(record.harness.as_str());
        create_private_dir(&dir)?;
        let path = self.path_of(&record.harness, &record.session);
        // The pid keeps two hooks for the same session, running at once, off each other's
        // temp file.
        let tmp = dir.join(format!(
            "{}.json.{}.tmp",
            record.session,
            std::process::id()
        ));

        write_private(&tmp, &record.to_json()).map_err(|err| Error::io("writing", &tmp, err))?;
        fs::rename(&tmp, &path).map_err(|err| {
            let _ = fs::remove_file(&tmp);
            Error::io("replacing", &path, err)
        })
    }

    /// The session's current record, or `None` if it has no file.
    pub fn read(
        &self,
        harness: &HarnessId,
        session: &SessionId,
    ) -> Result<Option<SessionRecord>, Error> {
        let path = self.path_of(harness, session);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(Error::io("reading", &path, err)),
        };
        SessionRecord::from_json(&bytes)
            .map(Some)
            .map_err(|source| Error::Record { path, source })
    }

    /// Every readable record, sorted by harness and then session, so two listings of a
    /// directory that hasn't changed compare equal. A file is skipped with a warning instead
    /// of failing the list when it can't be read or parsed — a newer format version, or junk —
    /// or when its record belongs at a different path, so a file's location can always be
    /// trusted to name its harness and session.
    pub fn list(&self) -> Result<Vec<SessionRecord>, Error> {
        let mut records = Vec::new();
        for dir in self.harness_dirs()? {
            for entry in entries(&dir)? {
                let path = entry.path();
                if !path.extension().is_some_and(|ext| ext == "json") {
                    continue; // temp files end in `.tmp`
                }
                let bytes = match fs::read(&path) {
                    Ok(bytes) => bytes,
                    // Deleted between listing and reading: that session just ended.
                    Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                    Err(err) => {
                        tracing::warn!("skipping {}: {err}", path.display());
                        continue;
                    }
                };
                let record = match SessionRecord::from_json(&bytes) {
                    Ok(record) => record,
                    Err(err) => {
                        tracing::warn!("skipping {}: {err}", path.display());
                        continue;
                    }
                };
                let expected = self.path_of(&record.harness, &record.session);
                if path != expected {
                    tracing::warn!(
                        "skipping {}: its record belongs at {}",
                        path.display(),
                        expected.display()
                    );
                    continue;
                }
                records.push(record);
            }
        }
        records.sort_by(|a, b| (&a.harness, &a.session).cmp(&(&b.harness, &b.session)));
        Ok(records)
    }

    /// Creates the state dir, readable only by the user, if it doesn't exist yet. Writing a
    /// record does this by itself; a reader that must watch the directory before any session
    /// has written needs it first.
    pub fn create_dir(&self) -> Result<(), Error> {
        create_private_dir(&self.dir)
    }

    /// Deletes the session's file. Not an error if it is already gone.
    pub fn remove(&self, harness: &HarnessId, session: &SessionId) -> Result<(), Error> {
        let path = self.path_of(harness, session);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::io("removing", &path, err)),
        }
    }

    /// Carries out a decision about one session's file, typically made from what
    /// [`Store::read`] returned.
    pub fn apply(&self, update: Update) -> Result<(), Error> {
        match update {
            Update::Write(record) => self.write(&record),
            Update::Skip => Ok(()),
            Update::Remove { harness, session } => self.remove(&harness, &session),
        }
    }

    /// Deletes session and temp files not modified for `max_age`, so sessions that crashed
    /// without ending — and temp files from a writer that died mid-write — don't accumulate.
    /// Judged by modification time, so it works on files it cannot parse too. Harness
    /// directories are kept, even when emptied. Returns how many files were deleted.
    pub fn prune(&self, max_age: Duration) -> Result<usize, Error> {
        let Some(cutoff) = SystemTime::now().checked_sub(max_age) else {
            return Ok(0);
        };
        let mut removed = 0;
        for dir in self.harness_dirs()? {
            for entry in entries(&dir)? {
                let path = entry.path();
                if !path
                    .extension()
                    .is_some_and(|ext| ext == "json" || ext == "tmp")
                {
                    continue;
                }
                let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
                    continue;
                };
                if modified >= cutoff {
                    continue;
                }
                match fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(Error::io("removing", &path, err)),
                }
            }
        }
        Ok(removed)
    }

    fn path_of(&self, harness: &HarnessId, session: &SessionId) -> PathBuf {
        self.dir
            .join(harness.as_str())
            .join(format!("{session}.json"))
    }

    /// Every directory in the state dir. Files lying directly in it belong to no harness and
    /// are never read or pruned.
    fn harness_dirs(&self) -> Result<Vec<PathBuf>, Error> {
        let mut dirs = Vec::new();
        for entry in entries(&self.dir)? {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                dirs.push(entry.path());
            }
        }
        Ok(dirs)
    }
}

/// The entries of `dir`, or none if it doesn't exist: a state dir no session has written to
/// yet, or a harness dir that vanished while being listed.
fn entries(dir: &Path) -> Result<Vec<fs::DirEntry>, Error> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io("listing", dir, err)),
    };
    listing
        .map(|entry| entry.map_err(|err| Error::io("listing", dir, err)))
        .collect()
}

/// Session files name the user's working directories, so only the user may read them. Any
/// missing parents are created with the same permissions.
fn create_private_dir(dir: &Path) -> Result<(), Error> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|err| Error::io("creating", dir, err))
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
}

/// What to do with one session's file. Deciding it is a pure function of the previous record
/// and whatever just happened; [`Store::apply`] does the I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// Replace the session's record with this one.
    Write(SessionRecord),
    /// Leave the file as it is, e.g. when the new record would say nothing new.
    Skip,
    /// Delete the session's file, e.g. when the session has ended.
    Remove {
        harness: HarnessId,
        session: SessionId,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot find the home directory")]
    NoHome,
    #[error("{action} {}: {source}", .path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: {source}", .path.display())]
    Record {
        path: PathBuf,
        #[source]
        source: record::Error,
    },
}

impl Error {
    fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_owned(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PetState;

    fn harness(id: &str) -> HarnessId {
        HarnessId::new(id).unwrap()
    }

    fn session(id: &str) -> SessionId {
        SessionId::new(id).unwrap()
    }

    fn record(harness_id: &str, session_id: &str, state: PetState) -> SessionRecord {
        SessionRecord::new(
            session(session_id),
            harness(harness_id),
            state,
            1_757_150_400,
        )
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn writes_then_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("sessions"));
        let written = record("claude-code", "s1", PetState::Writing);

        store.write(&written).unwrap();

        assert_eq!(
            store.read(&written.harness, &written.session).unwrap(),
            Some(written)
        );
        assert_eq!(file_names(store.dir()), ["claude-code"]);
        assert_eq!(
            file_names(&store.dir().join("claude-code")),
            ["s1.json"],
            "no temp file left behind"
        );
    }

    #[test]
    fn write_replaces_the_previous_record() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());

        store
            .write(&record("claude-code", "s1", PetState::Thinking))
            .unwrap();
        store
            .write(&record("claude-code", "s1", PetState::Proud))
            .unwrap();

        let read = store.read(&harness("claude-code"), &session("s1")).unwrap();
        assert_eq!(read.unwrap().state, PetState::Proud);
    }

    #[test]
    fn one_session_id_under_two_harnesses_is_two_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());

        store
            .write(&record("claude-code", "s1", PetState::Writing))
            .unwrap();
        store
            .write(&record("opencode", "s1", PetState::Proud))
            .unwrap();

        let state_under = |harness_id| {
            let read = store.read(&harness(harness_id), &session("s1")).unwrap();
            read.unwrap().state
        };
        assert_eq!(state_under("claude-code"), PetState::Writing);
        assert_eq!(state_under("opencode"), PetState::Proud);
        assert_eq!(store.list().unwrap().len(), 2);
    }

    #[test]
    fn reading_a_missing_session_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("never-created"));
        assert_eq!(
            store.read(&harness("claude-code"), &session("s1")).unwrap(),
            None
        );
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn list_skips_temp_junk_unknown_versions_and_loose_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        store
            .write(&record("claude-code", "good", PetState::Viewing))
            .unwrap();
        let dir = tmp.path().join("claude-code");
        fs::write(dir.join("good.json.123.tmp"), b"{half").unwrap();
        fs::write(dir.join("junk.json"), b"not json").unwrap();
        fs::write(
            dir.join("future.json"),
            br#"{"v":9,"session":"future","harness":"claude-code","state":"proud","ts":1}"#,
        )
        .unwrap();
        // A file outside any harness directory, as the layout before harness directories left.
        fs::write(
            tmp.path().join("loose.json"),
            record("claude-code", "loose", PetState::Proud).to_json(),
        )
        .unwrap();

        let listed = store.list().unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session.as_str(), "good");
    }

    #[test]
    fn list_skips_a_record_filed_at_the_wrong_path() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        let misfiled = record("claude-code", "s1", PetState::Thinking).to_json();
        for (dir, file) in [("opencode", "s1.json"), ("claude-code", "s2.json")] {
            fs::create_dir_all(tmp.path().join(dir)).unwrap();
            fs::write(tmp.path().join(dir).join(file), &misfiled).unwrap();
        }

        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn list_is_sorted_by_harness_then_session() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        for (harness_id, session_id) in [
            ("opencode", "a"),
            ("claude-code", "b"),
            ("claude-code", "a"),
        ] {
            store
                .write(&record(harness_id, session_id, PetState::Thinking))
                .unwrap();
        }

        let listed: Vec<_> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|record| format!("{}/{}", record.harness, record.session))
            .collect();

        assert_eq!(listed, ["claude-code/a", "claude-code/b", "opencode/a"]);
    }

    #[test]
    fn remove_tolerates_a_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());

        store
            .write(&record("claude-code", "s1", PetState::Offline))
            .unwrap();
        store
            .remove(&harness("claude-code"), &session("s1"))
            .unwrap();
        store
            .remove(&harness("claude-code"), &session("s1"))
            .unwrap();

        assert_eq!(
            store.read(&harness("claude-code"), &session("s1")).unwrap(),
            None
        );
    }

    #[test]
    fn prune_deletes_only_old_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        store
            .write(&record("claude-code", "old", PetState::Thinking))
            .unwrap();
        store
            .write(&record("claude-code", "fresh", PetState::Thinking))
            .unwrap();
        let dir = tmp.path().join("claude-code");
        fs::write(dir.join("old.json.99.tmp"), b"{").unwrap();
        fs::write(dir.join("notes.txt"), b"not ours").unwrap();
        let long_ago = SystemTime::now() - PRUNE_AFTER - Duration::from_secs(60);
        for name in ["old.json", "old.json.99.tmp", "notes.txt"] {
            let file = fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap();
            file.set_modified(long_ago).unwrap();
        }

        assert_eq!(store.prune(PRUNE_AFTER).unwrap(), 2);
        assert_eq!(file_names(&dir), ["fresh.json", "notes.txt"]);
    }

    #[test]
    fn apply_writes_skips_and_removes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        let state = || {
            let read = store.read(&harness("claude-code"), &session("s1")).unwrap();
            read.map(|record| record.state)
        };

        store
            .apply(Update::Write(record(
                "claude-code",
                "s1",
                PetState::Writing,
            )))
            .unwrap();
        assert_eq!(state(), Some(PetState::Writing));

        store.apply(Update::Skip).unwrap();
        assert_eq!(state(), Some(PetState::Writing));

        store
            .apply(Update::Remove {
                harness: harness("claude-code"),
                session: session("s1"),
            })
            .unwrap();
        assert_eq!(state(), None);
    }

    #[cfg(unix)]
    #[test]
    fn only_the_user_can_read_session_files() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("sessions"));
        store
            .write(&record("claude-code", "s1", PetState::Thinking))
            .unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(store.dir()), 0o700);
        assert_eq!(mode(&store.dir().join("claude-code")), 0o700);
        assert_eq!(
            mode(&store.dir().join("claude-code").join("s1.json")),
            0o600
        );
    }
}
