//! Configuring this machine's agent harnesses to run `remi-hook`: one file per harness, each
//! owning the edits to that harness's own configuration. The files belong to the user, so an
//! edit touches only what remi added, and a rewritten file keeps its previous version beside
//! it.

pub mod claude;
pub mod codex;
mod json_hooks;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// What `setup` or `uninstall` did to one harness's configuration file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub path: PathBuf,
    /// remi's hooks taken out, including earlier ones that were then written again.
    pub removed: usize,
    pub added: usize,
    pub saved: Saved,
}

/// Whether an edit had to rewrite its file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Saved {
    /// The file already said exactly this, so it was left as it was.
    Unchanged,
    Written {
        /// Where the file's previous contents were saved. `None` when there was no file yet.
        backup: Option<PathBuf>,
    },
}

/// Reads a JSON settings file. One that doesn't exist yet reads as an empty object.
fn read_settings(path: &Path) -> Result<Value, Error> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Value::Object(Map::new())),
        Err(err) => return Err(Error::io("reading", path, err)),
    };
    serde_json::from_slice(&bytes).map_err(|source| Error::Json {
        path: path.to_owned(),
        source,
    })
}

/// Replaces the settings file at `path` with `settings`, unless that equals `before`, the
/// contents as read. The new file is written beside the old one and renamed over it, so a
/// harness starting at that moment never reads half a file, and the old contents are first
/// copied to `<name>.bak`.
fn save_settings(path: &Path, before: &Value, settings: &Value) -> Result<Saved, Error> {
    if settings == before {
        return Ok(Saved::Unchanged);
    }

    // A settings file that is a symlink — say, into a dotfiles repository — is written through,
    // so the link survives.
    let target = match fs::canonicalize(path) {
        Ok(real) => real,
        Err(err) if err.kind() == io::ErrorKind::NotFound => path.to_owned(),
        Err(err) => return Err(Error::io("resolving", path, err)),
    };
    let dir = target
        .parent()
        .expect("a settings file path names a file inside a directory");
    fs::create_dir_all(dir).map_err(|err| Error::io("creating", dir, err))?;
    let existing = match fs::metadata(&target) {
        Ok(metadata) => Some(metadata),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(Error::io("reading", &target, err)),
    };

    let mut json = serde_json::to_string_pretty(settings).expect("a JSON value always serializes");
    json.push('\n');
    let name = target
        .file_name()
        .expect("a settings file path names a file")
        .to_string_lossy()
        .into_owned();
    let tmp = target.with_file_name(format!("{name}.remi-{}.tmp", std::process::id()));
    write_new_file(&tmp, json.as_bytes()).map_err(|err| Error::io("writing", &tmp, err))?;

    let backup = match &existing {
        Some(metadata) => {
            let backup = target.with_file_name(format!("{name}.bak"));
            let kept = fs::set_permissions(&tmp, metadata.permissions())
                .map_err(|err| Error::io("writing", &tmp, err))
                .and_then(|()| {
                    fs::copy(&target, &backup)
                        .map(drop)
                        .map_err(|err| Error::io("backing up to", &backup, err))
                });
            if let Err(err) = kept {
                let _ = fs::remove_file(&tmp);
                return Err(err);
            }
            Some(backup)
        }
        None => None,
    };

    fs::rename(&tmp, &target).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        Error::io("replacing", &target, err)
    })?;
    Ok(Saved::Written { backup })
}

/// Creates a file only the user can read, since settings files can hold credentials. A file
/// that replaces an existing one gets that file's permissions afterwards.
fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
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
    #[error("{} is not valid JSON: {source}", .path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The file is JSON, but not shaped the way the harness documents, so remi won't guess
    /// where its hooks go.
    #[error("{}: {what}", .path.display())]
    Shape { path: PathBuf, what: String },
    #[error("this program's path {} is not valid UTF-8, so no hook command can name it", .0.display())]
    PathNotUtf8(PathBuf),
}

impl Error {
    fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_owned(),
            source,
        }
    }

    fn shape(path: &Path, what: String) -> Self {
        Self::Shape {
            path: path.to_owned(),
            what,
        }
    }
}
