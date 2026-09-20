//! Claude Code's event mapping and user settings location.
use super::{
    Edit, Error,
    json_hooks::{self, Hook, Inspection},
};
use crate::{Harness, SignalEvent};
use std::path::{Path, PathBuf};
const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Every hook remi installs, in the order `setup` writes them.
pub(super) const HOOKS: &[Hook] = &[
    Hook {
        event: "UserPromptSubmit",
        matcher: None,
        signal: SignalEvent::TurnStart,
        timeout: 5,
    },
    Hook {
        event: "PreToolUse",
        matcher: Some("Read|Grep|Glob"),
        signal: SignalEvent::ReadStart,
        timeout: 5,
    },
    Hook {
        event: "PreToolUse",
        matcher: Some("Edit|Write"),
        signal: SignalEvent::EditStart,
        timeout: 5,
    },
    // Every tool, not only those that can prompt: a tool finishing is the only sign Claude Code
    // gives that a prompt was approved, whichever tool asked.
    Hook {
        event: "PostToolUse",
        matcher: Some("*"),
        signal: SignalEvent::ToolEnd,
        timeout: 5,
    },
    // `PostToolUse` runs only for tools that succeed.
    Hook {
        event: "PostToolUseFailure",
        matcher: Some("*"),
        signal: SignalEvent::ToolEnd,
        timeout: 5,
    },
    // Runs the moment a permission prompt appears.
    Hook {
        event: "PermissionRequest",
        matcher: Some("*"),
        signal: SignalEvent::ApprovalAsked,
        timeout: 5,
    },
    // Only these types mean Claude is blocked on the user; others, such as `idle_prompt` or
    // `auth_success`, would show the waiting pose for nothing. `permission_prompt` comes about
    // six seconds after the prompt appears, but it is also the only signal for a sandboxed
    // command asking to use the network, which `PermissionRequest` doesn't cover.
    Hook {
        event: "Notification",
        matcher: Some(
            "permission_prompt|agent_needs_input|elicitation_dialog|elicitation_url_dialog",
        ),
        signal: SignalEvent::ApprovalAsked,
        timeout: 5,
    },
    // Runs for each piece of reply text as it streams, and Claude Code holds that piece on
    // screen until the hook returns, so a stuck hook gets cut off quickly.
    Hook {
        event: "MessageDisplay",
        matcher: None,
        signal: SignalEvent::Reply,
        timeout: 2,
    },
    Hook {
        event: "Stop",
        matcher: None,
        signal: SignalEvent::TurnEnd,
        timeout: 5,
    },
    // Runs instead of `Stop` when a turn ends on an API error, such as a rate limit. Without it
    // the pose would stay wherever the error found it.
    Hook {
        event: "StopFailure",
        matcher: None,
        signal: SignalEvent::TurnEnd,
        timeout: 5,
    },
    Hook {
        event: "SessionEnd",
        matcher: None,
        signal: SignalEvent::SessionEnd,
        timeout: 5,
    },
];

/// Claude Code's user settings file: in `$CLAUDE_CONFIG_DIR` when that is set, as Claude Code
/// itself does, otherwise in `<home>/.claude`.
pub fn settings_path() -> Result<PathBuf, Error> {
    if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("settings.json"));
    }
    let base = directories::BaseDirs::new().ok_or(Error::NoHome)?;
    Ok(base.home_dir().join(".claude").join("settings.json"))
}

pub fn install(path: &Path, hook_path: &Path) -> Result<Edit, Error> {
    json_hooks::install(path, hook_path, Harness::ClaudeCode, HOOKS)
}

pub fn inspect(path: &Path, hook_path: &Path) -> Result<Inspection, Error> {
    json_hooks::inspect(path, hook_path, Harness::ClaudeCode, HOOKS)
}

pub use super::json_hooks::uninstall;
