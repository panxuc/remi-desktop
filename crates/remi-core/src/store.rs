use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::record::{self, SessionId, SessionRecord};

/// Overrides the state directory. Meant for tests.
pub const STATE_DIR_ENV: &str = "REMI_STATE_DIR";

/// How long a session file may go unwritten before [`Store::prune`] deletes it: long enough
/// that no live session is ever affected, short enough that crashed ones don't pile up.
pub const PRUNE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The directory of session state files on one machine: one `<session id>.json` per session,
/// each holding that session's current [`SessionRecord`].
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
        self.create_dir()?;
        let path = self.path_of(&record.session);
        // The pid keeps two hooks for the same session, running at once, off each other's
        // temp file.
        let tmp = self.dir.join(format!(
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
    pub fn read(&self, id: &SessionId) -> Result<Option<SessionRecord>, Error> {
        let path = self.path_of(id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(Error::io("reading", &path, err)),
        };
        SessionRecord::from_json(&bytes)
            .map(Some)
            .map_err(|source| Error::Record { path, source })
    }

    /// Every readable record, in no particular order. A file that can't be read or parsed — a
    /// newer format version, or junk — is skipped with a warning instead of failing the list.
    pub fn list(&self) -> Result<Vec<SessionRecord>, Error> {
        let Some(entries) = self.entries()? else {
            return Ok(Vec::new());
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|err| Error::io("listing", &self.dir, err))?
                .path();
            if !path.extension().is_some_and(|ext| ext == "json") {
                continue; // temp files end in `.tmp`
            }
            match fs::read(&path) {
                Ok(bytes) => match SessionRecord::from_json(&bytes) {
                    Ok(record) => records.push(record),
                    Err(err) => tracing::warn!("skipping {}: {err}", path.display()),
                },
                // Deleted between listing and reading: that session just ended.
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => tracing::warn!("skipping {}: {err}", path.display()),
            }
        }
        Ok(records)
    }

    /// Deletes the session's file. Not an error if it is already gone.
    pub fn remove(&self, id: &SessionId) -> Result<(), Error> {
        let path = self.path_of(id);
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
            Update::Remove(id) => self.remove(&id),
        }
    }

    /// Deletes session and temp files not modified for `max_age`, so sessions that crashed
    /// without ending — and temp files from a writer that died mid-write — don't accumulate.
    /// Judged by modification time, so it works on files it cannot parse too. Returns how
    /// many files were deleted.
    pub fn prune(&self, max_age: Duration) -> Result<usize, Error> {
        let Some(cutoff) = SystemTime::now().checked_sub(max_age) else {
            return Ok(0);
        };
        let Some(entries) = self.entries()? else {
            return Ok(0);
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry.map_err(|err| Error::io("listing", &self.dir, err))?;
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
        Ok(removed)
    }

    fn path_of(&self, id: &SessionId) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// `None` if the directory doesn't exist yet, which just means no session has written.
    fn entries(&self) -> Result<Option<fs::ReadDir>, Error> {
        match fs::read_dir(&self.dir) {
            Ok(entries) => Ok(Some(entries)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::io("listing", &self.dir, err)),
        }
    }

    /// Session files name the user's working directories, so only the user may read them.
    fn create_dir(&self) -> Result<(), Error> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(&self.dir)
            .map_err(|err| Error::io("creating", &self.dir, err))
    }
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
    Remove(SessionId),
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

    fn record(id: &str, state: PetState) -> SessionRecord {
        SessionRecord::new(
            SessionId::new(id).unwrap(),
            "claude-code",
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
        let written = record("s1", PetState::Writing);

        store.write(&written).unwrap();

        assert_eq!(store.read(&written.session).unwrap(), Some(written));
        assert_eq!(
            file_names(store.dir()),
            ["s1.json"],
            "no temp file left behind"
        );
    }

    #[test]
    fn write_replaces_the_previous_record() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());

        store.write(&record("s1", PetState::Thinking)).unwrap();
        store.write(&record("s1", PetState::Proud)).unwrap();

        let id = SessionId::new("s1").unwrap();
        assert_eq!(store.read(&id).unwrap().unwrap().state, PetState::Proud);
    }

    #[test]
    fn reading_a_missing_session_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("never-created"));
        assert_eq!(store.read(&SessionId::new("s1").unwrap()).unwrap(), None);
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn list_skips_temp_junk_and_unknown_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        store.write(&record("good", PetState::Viewing)).unwrap();
        fs::write(tmp.path().join("good.json.123.tmp"), b"{half").unwrap();
        fs::write(tmp.path().join("junk.json"), b"not json").unwrap();
        fs::write(
            tmp.path().join("future.json"),
            br#"{"v":9,"session":"future","harness":"x","state":"proud","ts":1}"#,
        )
        .unwrap();

        let listed = store.list().unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session.as_str(), "good");
    }

    #[test]
    fn remove_tolerates_a_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        let id = SessionId::new("s1").unwrap();

        store.write(&record("s1", PetState::Offline)).unwrap();
        store.remove(&id).unwrap();
        store.remove(&id).unwrap();

        assert_eq!(store.read(&id).unwrap(), None);
    }

    #[test]
    fn prune_deletes_only_old_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        store.write(&record("old", PetState::Thinking)).unwrap();
        store.write(&record("fresh", PetState::Thinking)).unwrap();
        fs::write(tmp.path().join("old.json.99.tmp"), b"{").unwrap();
        fs::write(tmp.path().join("notes.txt"), b"not ours").unwrap();
        let long_ago = SystemTime::now() - PRUNE_AFTER - Duration::from_secs(60);
        for name in ["old.json", "old.json.99.tmp", "notes.txt"] {
            let file = fs::File::options()
                .write(true)
                .open(tmp.path().join(name))
                .unwrap();
            file.set_modified(long_ago).unwrap();
        }

        assert_eq!(store.prune(PRUNE_AFTER).unwrap(), 2);
        assert_eq!(file_names(tmp.path()), ["fresh.json", "notes.txt"]);
    }

    #[test]
    fn apply_writes_skips_and_removes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path());
        let id = SessionId::new("s1").unwrap();

        store
            .apply(Update::Write(record("s1", PetState::Writing)))
            .unwrap();
        assert_eq!(store.read(&id).unwrap().unwrap().state, PetState::Writing);

        store.apply(Update::Skip).unwrap();
        assert_eq!(store.read(&id).unwrap().unwrap().state, PetState::Writing);

        store.apply(Update::Remove(id.clone())).unwrap();
        assert_eq!(store.read(&id).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn only_the_user_can_read_session_files() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("sessions"));
        store.write(&record("s1", PetState::Thinking)).unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(store.dir()), 0o700);
        assert_eq!(mode(&store.dir().join("s1.json")), 0o600);
    }
}
