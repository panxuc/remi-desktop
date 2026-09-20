//! Shared JSON hook editor for Claude Code and Codex. Edits only Remi commands, preserving
//! unrelated settings and hooks, backups, and idempotent installation.

use super::{Edit, Error, read_settings, save_settings};
use crate::{Harness, SignalEvent};
use clap::ValueEnum;
use serde_json::{Map, Value, json};
use std::ffi::OsStr;
use std::fmt;
use std::path::Path;

pub(super) struct Hook {
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub signal: SignalEvent,
    pub timeout: u64,
}

impl Hook {
    /// This hook as it appears in the settings file, running `program`.
    fn entry(&self, program: &str, harness: Harness) -> HookEntry {
        HookEntry {
            event: self.event.to_owned(),
            matcher: self.matcher.map(str::to_owned),
            command: format!(
                "{program} signal {} --harness {}",
                cli_word(&self.signal),
                cli_word(&harness)
            ),
        }
    }
}

/// A remi hook as it appears in the settings file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookEntry {
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
}

impl fmt::Display for HookEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.event)?;
        if let Some(matcher) = &self.matcher {
            write!(f, " [{matcher}]")?;
        }
        write!(f, ": {}", self.command)
    }
}

/// How the remi hooks in a settings file compare with what `setup` would write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    /// How many hooks `setup` writes.
    pub expected: usize,
    /// Hooks `setup` would write that the file lacks.
    pub missing: Vec<HookEntry>,
    /// remi hooks in the file that `setup` wouldn't write: from an older version, or running a
    /// different copy of `remi-hook`.
    pub unexpected: Vec<HookEntry>,
}

/// Makes the settings file at `path` hold exactly remi's hooks, running `hook_path`, and
/// leaves everything else in it alone. Earlier remi hooks are replaced, so running it again —
/// or after moving `remi-hook` — is safe.
pub fn install(
    path: &Path,
    hook_path: &Path,
    harness: Harness,
    hooks: &[Hook],
) -> Result<Edit, Error> {
    let program = program(hook_path)?;
    let before = read_settings(path)?;
    let mut settings = before.clone();
    let removed = remove_remi_hooks(&mut settings).map_err(|what| Error::shape(path, what))?;
    let added = add_hooks(&mut settings, &program, harness, hooks)
        .map_err(|what| Error::shape(path, what))?;
    let saved = save_settings(path, &before, &settings)?;
    Ok(Edit {
        path: path.to_owned(),
        removed,
        added,
        saved,
    })
}

/// Takes every remi hook out of the settings file at `path`, and leaves everything else in it
/// alone.
pub fn uninstall(path: &Path) -> Result<Edit, Error> {
    let before = read_settings(path)?;
    let mut settings = before.clone();
    let removed = remove_remi_hooks(&mut settings).map_err(|what| Error::shape(path, what))?;
    let saved = save_settings(path, &before, &settings)?;
    Ok(Edit {
        path: path.to_owned(),
        removed,
        added: 0,
        saved,
    })
}

/// Compares the remi hooks in the settings file at `path` with what `setup` would write when
/// run from `hook_path`.
pub fn inspect(
    path: &Path,
    hook_path: &Path,
    harness: Harness,
    hooks: &[Hook],
) -> Result<Inspection, Error> {
    let program = program(hook_path)?;
    let settings = read_settings(path)?;
    let found = remi_hooks(&settings).map_err(|what| Error::shape(path, what))?;
    let expected: Vec<HookEntry> = hooks
        .iter()
        .map(|hook| hook.entry(&program, harness))
        .collect();
    let missing = expected
        .iter()
        .filter(|hook| !found.contains(hook))
        .cloned()
        .collect();
    let unexpected = found
        .into_iter()
        .filter(|hook| !expected.contains(hook))
        .collect();
    Ok(Inspection {
        expected: expected.len(),
        missing,
        unexpected,
    })
}

/// Removes every remi hook, then every group and event list that only remi's hooks were in,
/// and `hooks` itself if that leaves it empty. Groups and events the user left empty stay.
/// Returns how many hooks were removed.
fn remove_remi_hooks(settings: &mut Value) -> Result<usize, String> {
    let root = settings.as_object_mut().ok_or(TOP_LEVEL_NOT_AN_OBJECT)?;
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(0);
    };
    let events = hooks.as_object_mut().ok_or(HOOKS_NOT_AN_OBJECT)?;

    let mut removed = 0;
    let mut emptied_events = Vec::new();
    for (event, groups) in events.iter_mut() {
        let groups = groups.as_array_mut().ok_or_else(|| not_a_list(event))?;
        let removed_before = removed;
        groups.retain_mut(|group| {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let count = entries.len();
            entries.retain(|entry| !is_remi_hook(entry));
            removed += count - entries.len();
            !entries.is_empty() || count == 0
        });
        if groups.is_empty() && removed > removed_before {
            emptied_events.push(event.clone());
        }
    }
    events.retain(|event, _| !emptied_events.contains(event));
    // `retain` rather than `remove`, which could reorder the keys around it.
    if events.is_empty() && !emptied_events.is_empty() {
        root.retain(|key, _| key != "hooks");
    }
    Ok(removed)
}

/// Appends one group per remi hook, running `program`. Returns how many hooks were added.
fn add_hooks(
    settings: &mut Value,
    program: &str,
    harness: Harness,
    hooks: &[Hook],
) -> Result<usize, String> {
    let root = settings.as_object_mut().ok_or(TOP_LEVEL_NOT_AN_OBJECT)?;
    let section = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let events = section.as_object_mut().ok_or(HOOKS_NOT_AN_OBJECT)?;
    for hook in hooks {
        let groups = events
            .entry(hook.event)
            .or_insert_with(|| Value::Array(Vec::new()));
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| not_a_list(hook.event))?;
        let entry = hook.entry(program, harness);
        let mut group = Map::new();
        if let Some(matcher) = entry.matcher {
            group.insert("matcher".to_owned(), Value::String(matcher));
        }
        group.insert(
            "hooks".to_owned(),
            json!([{ "type": "command", "command": entry.command, "timeout": hook.timeout }]),
        );
        groups.push(Value::Object(group));
    }
    Ok(hooks.len())
}

/// Every remi hook in `settings`, in file order.
fn remi_hooks(settings: &Value) -> Result<Vec<HookEntry>, String> {
    let root = settings.as_object().ok_or(TOP_LEVEL_NOT_AN_OBJECT)?;
    let Some(hooks) = root.get("hooks") else {
        return Ok(Vec::new());
    };
    let events = hooks.as_object().ok_or(HOOKS_NOT_AN_OBJECT)?;
    let mut found = Vec::new();
    for (event, groups) in events {
        for group in groups.as_array().ok_or_else(|| not_a_list(event))? {
            let matcher = group.get("matcher").and_then(Value::as_str);
            let Some(entries) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for entry in entries.iter().filter(|entry| is_remi_hook(entry)) {
                found.push(HookEntry {
                    event: event.clone(),
                    matcher: matcher.map(str::to_owned),
                    command: entry["command"].as_str().unwrap_or_default().to_owned(),
                });
            }
        }
    }
    Ok(found)
}

const TOP_LEVEL_NOT_AN_OBJECT: &str = "the top level is not a JSON object";
const HOOKS_NOT_AN_OBJECT: &str = "`hooks` is not an object";

fn not_a_list(event: &str) -> String {
    format!("`hooks.{event}` is not a list")
}

fn is_remi_hook(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(runs_remi_signal)
}

/// Whether `command` runs a program named `remi-hook` with `signal` as its first argument. The
/// program may be a bare name or a path, and may be quoted; a quoted path that itself contains
/// the quote character is not recognised.
fn runs_remi_signal(command: &str) -> bool {
    let command = command.trim_start();
    let (program, rest) = match command.chars().next() {
        Some(quote @ ('\'' | '"')) => match command[1..].split_once(quote) {
            Some(split) => split,
            None => return false,
        },
        _ => command
            .split_once(char::is_whitespace)
            .unwrap_or((command, "")),
    };
    let named_remi_hook = Path::new(program)
        .file_name()
        .is_some_and(|name| name == OsStr::new("remi-hook") || name == OsStr::new("remi-hook.exe"));
    named_remi_hook && rest.split_whitespace().next() == Some("signal")
}

/// `hook_path` as the first word of a shell command: as it is when every character is safe,
/// otherwise in single quotes.
fn program(hook_path: &Path) -> Result<String, Error> {
    let path = hook_path
        .to_str()
        .ok_or_else(|| Error::PathNotUtf8(hook_path.to_owned()))?;
    let plain = !path.is_empty()
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-+:@%,".contains(&byte));
    Ok(if plain {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    })
}

/// How `value` is spelled on the command line, so a hook command always names an event the
/// same way `remi-hook` parses it.
fn cli_word(value: &impl ValueEnum) -> String {
    value
        .to_possible_value()
        .expect("no command-line value is hidden")
        .get_name()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::claude::{HOOKS, inspect, install};

    fn add_hooks(settings: &mut Value, program: &str) -> Result<usize, String> {
        super::add_hooks(settings, program, Harness::ClaudeCode, HOOKS)
    }

    const PROGRAM: &str = "/usr/local/bin/remi-hook";

    fn commands(settings: &Value) -> Vec<String> {
        remi_hooks(settings)
            .unwrap()
            .into_iter()
            .map(|hook| hook.command)
            .collect()
    }

    #[test]
    fn recognises_commands_that_run_remi_hook_signal() {
        for yes in [
            "remi-hook signal turn-start --harness claude-code",
            "/home/u/.local/bin/remi-hook signal tool-end --harness claude-code",
            "'/Applications/Remi Desktop.app/Contents/MacOS/remi-hook' signal reply",
            "\"/opt/remi hook/remi-hook\" signal turn-end",
            "  remi-hook   signal",
            "C:/Tools/remi-hook.exe signal turn-start",
        ] {
            assert!(runs_remi_signal(yes), "{yes}");
        }
        for no in [
            "remi-hook watch",
            "remi-hook",
            "my-remi-hook signal turn-start",
            "echo remi-hook signal",
            "'/unterminated/remi-hook signal",
            "~/scripts/notify.sh",
        ] {
            assert!(!runs_remi_signal(no), "{no}");
        }
    }

    #[test]
    fn quotes_a_program_path_only_when_it_needs_it() {
        let quoted = |path: &str| program(Path::new(path)).unwrap();
        assert_eq!(
            quoted("/home/u/.local/bin/remi-hook"),
            "/home/u/.local/bin/remi-hook"
        );
        assert_eq!(
            quoted("/Applications/Remi Desktop.app/remi-hook"),
            "'/Applications/Remi Desktop.app/remi-hook'"
        );
        assert_eq!(quoted("/it's/remi-hook"), r"'/it'\''s/remi-hook'");
    }

    #[test]
    fn writes_every_hook_into_an_empty_file_in_order() {
        let mut settings = json!({});

        assert_eq!(remove_remi_hooks(&mut settings).unwrap(), 0);
        assert_eq!(add_hooks(&mut settings, PROGRAM).unwrap(), HOOKS.len());

        let expected: Vec<HookEntry> = HOOKS
            .iter()
            .map(|hook| hook.entry(PROGRAM, Harness::ClaudeCode))
            .collect();
        assert_eq!(remi_hooks(&settings).unwrap(), expected);
        assert_eq!(
            settings["hooks"]["PreToolUse"][1],
            json!({
                "matcher": "Edit|Write",
                "hooks": [{
                    "type": "command",
                    "command": "/usr/local/bin/remi-hook signal edit-start --harness claude-code",
                    "timeout": 5
                }]
            })
        );
        assert_eq!(
            settings["hooks"]["MessageDisplay"][0]["hooks"][0]["timeout"],
            2
        );
    }

    #[test]
    fn replaces_earlier_remi_hooks_and_keeps_the_users_own() {
        let mut settings = json!({
            "model": "opus",
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [
                        { "type": "command", "command": "~/scripts/log-prompt.sh" },
                        { "type": "command", "command": "remi-hook signal turn-start --harness claude-code", "timeout": 5 }
                    ]
                }],
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{ "type": "command", "command": "~/scripts/guard.sh" }]
                }],
                "SessionStart": [{
                    "hooks": [{ "type": "command", "command": "remi-hook signal session-start --harness claude-code" }]
                }]
            }
        });

        assert_eq!(remove_remi_hooks(&mut settings).unwrap(), 2);
        add_hooks(&mut settings, PROGRAM).unwrap();

        let hooks = &settings["hooks"];
        assert_eq!(
            hooks["UserPromptSubmit"][0]["hooks"],
            json!([{ "type": "command", "command": "~/scripts/log-prompt.sh" }])
        );
        assert_eq!(hooks["PreToolUse"][0]["matcher"], "Bash");
        assert!(hooks.get("SessionStart").is_none(), "{hooks}");
        assert!(commands(&settings).iter().all(|c| c.starts_with(PROGRAM)));
        assert_eq!(commands(&settings).len(), HOOKS.len());
    }

    #[test]
    fn installing_twice_gives_the_same_settings() {
        let mut once = json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "~/done.sh" }] }] } });
        remove_remi_hooks(&mut once).unwrap();
        add_hooks(&mut once, PROGRAM).unwrap();

        let mut twice = once.clone();
        remove_remi_hooks(&mut twice).unwrap();
        add_hooks(&mut twice, PROGRAM).unwrap();

        assert_eq!(twice, once);
    }

    #[test]
    fn uninstall_drops_only_what_remi_emptied() {
        let mut settings = json!({
            "theme": "auto",
            "hooks": {
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "remi-hook signal turn-end --harness claude-code" }] },
                    { "hooks": [] }
                ],
                "SessionEnd": [
                    { "hooks": [{ "type": "command", "command": "remi-hook signal session-end --harness claude-code" }] }
                ],
                "PreCompact": []
            }
        });

        assert_eq!(remove_remi_hooks(&mut settings).unwrap(), 2);

        assert_eq!(
            settings,
            json!({ "theme": "auto", "hooks": { "Stop": [{ "hooks": [] }], "PreCompact": [] } })
        );
    }

    #[test]
    fn uninstall_drops_hooks_when_only_remi_was_in_it() {
        let mut settings = json!({ "model": "opus", "theme": "auto" });
        add_hooks(&mut settings, PROGRAM).unwrap();

        remove_remi_hooks(&mut settings).unwrap();

        assert_eq!(settings, json!({ "model": "opus", "theme": "auto" }));
    }

    #[test]
    fn keys_the_edit_never_touched_keep_their_order() {
        // The user's own hook keeps `hooks` in the file once remi's are gone.
        let mut settings: Value = serde_json::from_str(
            r#"{"theme":"auto","hooks":{"Stop":[{"hooks":[{"type":"command","command":"~/done.sh"}]}]},"model":"opus","verbose":false}"#,
        )
        .unwrap();

        add_hooks(&mut settings, PROGRAM).unwrap();
        remove_remi_hooks(&mut settings).unwrap();

        let keys: Vec<&str> = settings
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["theme", "hooks", "model", "verbose"]);
    }

    #[test]
    fn refuses_settings_of_the_wrong_shape() {
        for bad in [
            json!([]),
            json!({ "hooks": "none" }),
            json!({ "hooks": { "Stop": {} } }),
        ] {
            let mut settings = bad.clone();
            assert!(remove_remi_hooks(&mut settings).is_err(), "{bad}");
            assert!(add_hooks(&mut settings, PROGRAM).is_err(), "{bad}");
            assert!(remi_hooks(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn inspect_reports_missing_and_unexpected_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install(&path, Path::new("/old/place/remi-hook")).unwrap();

        let inspection = inspect(&path, Path::new(PROGRAM)).unwrap();

        assert_eq!(inspection.expected, HOOKS.len());
        assert_eq!(inspection.missing.len(), HOOKS.len());
        assert_eq!(inspection.unexpected.len(), HOOKS.len());
        assert!(
            inspection.unexpected[0]
                .command
                .starts_with("/old/place/remi-hook")
        );

        install(&path, Path::new(PROGRAM)).unwrap();
        let inspection = inspect(&path, Path::new(PROGRAM)).unwrap();
        assert!(inspection.missing.is_empty() && inspection.unexpected.is_empty());
    }
}
