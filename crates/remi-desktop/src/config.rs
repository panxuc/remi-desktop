//! The pet's config file (plan §5.4), and the debounced writer that keeps it current.
//!
//! Nothing here is allowed to stop the pet starting. A config file that is missing, unreadable or
//! malformed logs and falls back to defaults: the alternative is a desktop pet that refuses to
//! appear because of a typo, which is worse than one that appears in the wrong place.

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use directories::ProjectDirs;
use remi_core::record::{HarnessId, SessionId};
use remi_core::registry::{Selection, SessionKey};
use remi_core::source::ConnectionId;
use serde::{Deserialize, Serialize};

/// How long a burst of changes is collected before the file is written. A window drag emits a
/// `Moved` event per frame; without this the config file would be rewritten sixty times a second.
const DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub renderer: Renderer,
    /// Never turned on by the pet itself: with the window click-through, only the tray can turn it
    /// back off (plan §5.2), so an automatic one would be a trap.
    pub click_through: bool,
    /// Multiplies the window's edge length. The renderer refits itself to whatever size it gets.
    pub scale: f32,
    pub selection: SelectionConfig,
    pub window: WindowConfig,
    pub connections: Vec<Connection>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            renderer: Renderer::Spine,
            click_through: false,
            scale: 1.0,
            selection: SelectionConfig::Auto,
            window: WindowConfig::default(),
            // A pet with no connections can never show anything, so the default is the one
            // connection that needs no configuring: this machine.
            connections: vec![Connection {
                name: "local".into(),
                kind: ConnectionKind::Local,
            }],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Renderer {
    #[default]
    Spine,
    Gif,
}

impl Renderer {
    /// Passed to the webview as a query parameter, which is how `ui/app.js` chooses (plan §5.3):
    /// Rust never learns which renderer is active, it only relays the user's choice.
    pub fn as_query(self) -> &'static str {
        match self {
            Renderer::Spine => "spine",
            Renderer::Gif => "gif",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    /// Logical, not physical, pixels: a position saved on a Retina display has to mean the same
    /// place when the pet is later opened on an external monitor.
    pub x: Option<f64>,
    pub y: Option<f64>,
}

/// `selection = "auto"`, or an inline table naming one session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectionConfig {
    Auto,
    #[serde(untagged)]
    Pinned {
        connection: String,
        harness: String,
        session: String,
    },
}

impl SelectionConfig {
    /// A pin naming a session id the current build would reject is dropped rather than refused:
    /// the file may have been written by a different version, and Auto is always a safe answer.
    pub fn to_selection(&self) -> Selection {
        match self {
            SelectionConfig::Auto => Selection::Auto,
            SelectionConfig::Pinned {
                connection,
                harness,
                session,
            } => {
                let key = HarnessId::new(harness.clone())
                    .ok()
                    .zip(SessionId::new(session.clone()).ok());
                match key {
                    Some((harness, session)) => Selection::Pinned(SessionKey {
                        connection: ConnectionId::new(connection.clone()),
                        harness,
                        session,
                    }),
                    None => {
                        tracing::warn!("ignoring unusable pinned selection in config");
                        Selection::Auto
                    }
                }
            }
        }
    }

    pub fn from_selection(selection: &Selection) -> Self {
        match selection {
            Selection::Auto => SelectionConfig::Auto,
            Selection::Pinned(key) => SelectionConfig::Pinned {
                connection: key.connection.to_string(),
                harness: key.harness.to_string(),
                session: key.session.to_string(),
            },
        }
    }
}

/// A *source*, not a host: the same machine reached two ways is two connections, and nothing
/// deduplicates them (plan §5.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub name: String,
    pub kind: ConnectionKind,
}

/// ⚠️ When `ssh` lands at M4 it needs `ssh_host` as a *sibling* of `kind` (plan §5.4), which is a
/// `#[serde(flatten)]`ed internally-tagged enum — and serde silently rejects every flattened field
/// when the outer struct also has `deny_unknown_fields`. One or the other has to give. Keeping the
/// typo-catching is worth more here than the tidier shape, so the kind-specific keys should become
/// named `Option` fields on [`Connection`] rather than a flattened enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionKind {
    /// This machine's own state dir. The only kind v1 implements; `ssh` is M4 and `mqtt` is M5.
    Local,
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("moe", "anything", "remi")?;
        Some(dirs.config_dir().join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            tracing::warn!("no config directory on this platform; using defaults");
            return Self::default();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("no config at {}; using defaults", path.display());
                return Self::default();
            }
            Err(err) => {
                tracing::warn!("reading {}: {err}; using defaults", path.display());
                return Self::default();
            }
        };
        match toml::from_str(&text) {
            Ok(config) => config,
            Err(err) => {
                // Deliberately not rewritten with the defaults: the user's file is left exactly as
                // it is so they can find the typo, rather than having it silently replaced.
                tracing::error!("parsing {}: {err}; using defaults", path.display());
                Self::default()
            }
        }
    }

    fn write(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no config directory on this platform")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| format!("serialising config: {e}"))?;
        std::fs::write(&path, text).map_err(|e| format!("writing {}: {e}", path.display()))
    }
}

/// Writes the config, coalescing bursts. Cheap to clone and to call from any thread.
#[derive(Clone)]
pub struct Saver(Sender<Config>);

impl Saver {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<Config>();
        std::thread::Builder::new()
            .name("remi-config".into())
            .spawn(move || {
                while let Ok(mut config) = rx.recv() {
                    // Keep taking whatever else arrives within the window; only the last one is
                    // worth writing, since each config is the whole file.
                    loop {
                        match rx.recv_timeout(DEBOUNCE) {
                            Ok(newer) => config = newer,
                            Err(RecvTimeoutError::Timeout) => break,
                            // The app is shutting down; write what we have rather than lose it.
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    if let Err(err) = config.write() {
                        tracing::error!("{err}");
                    }
                }
            })
            .expect("spawning the config writer");
        Self(tx)
    }

    /// Queues a write. Losing one because the writer has gone is not worth reporting: it only
    /// happens on shutdown, where the next start reads the last file that did land.
    pub fn save(&self, config: Config) {
        let _ = self.0.send(config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn reads_the_documented_file_shape() {
        // This is plan §5.4's example, minus the connection kinds v1 does not implement yet.
        let text = r#"
            renderer = "spine"
            click_through = false
            scale = 1.0
            selection = "auto"

            [window]
            x = 1620
            y = 820

            [[connections]]
            name = "local"
            kind = "local"
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.renderer, Renderer::Spine);
        assert_eq!(config.window.x, Some(1620.0));
        assert_eq!(config.selection.to_selection(), Selection::Auto);
        assert_eq!(config.connections.len(), 1);
    }

    #[test]
    fn a_pinned_selection_round_trips() {
        let text = r#"selection = { connection = "plume", harness = "claude-code", session = "a1b2" }"#;
        let config: Config = toml::from_str(text).unwrap();
        let selection = config.selection.to_selection();
        assert!(matches!(&selection, Selection::Pinned(key) if key.session.as_str() == "a1b2"));
        assert_eq!(SelectionConfig::from_selection(&selection), config.selection);
    }

    #[test]
    fn a_pin_the_current_build_cannot_parse_falls_back_to_auto() {
        let text = r#"selection = { connection = "plume", harness = "Claude Code", session = "../x" }"#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.selection.to_selection(), Selection::Auto);
    }
}
