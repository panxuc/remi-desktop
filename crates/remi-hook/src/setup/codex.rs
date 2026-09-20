//! Codex lifecycle hooks: https://learn.chatgpt.com/docs/hooks
//!
//! Use the supported stdin envelope, never the unstable transcript format. Codex loads
//! hooks.json beside its user config; setup leaves config.toml and hook trust to Codex.
use std::path::{Path, PathBuf};

use super::{
    Edit, Error,
    json_hooks::{self, Hook, Inspection},
};
use crate::{Harness, SignalEvent};

const HOOKS: &[Hook] = &[
    Hook {
        event: "UserPromptSubmit",
        matcher: None,
        signal: SignalEvent::TurnStart,
        timeout: 5,
    },
    Hook {
        event: "PreToolUse",
        matcher: Some("^(Read|Grep|Glob|read_file|list_dir|grep_files|view_image)$"),
        signal: SignalEvent::ReadStart,
        timeout: 5,
    },
    Hook {
        event: "PreToolUse",
        matcher: Some("^(apply_patch|Edit|Write)$"),
        signal: SignalEvent::EditStart,
        timeout: 5,
    },
    Hook {
        event: "PreToolUse",
        matcher: Some("^request_user_input$"),
        signal: SignalEvent::ApprovalAsked,
        timeout: 5,
    },
    Hook {
        event: "PermissionRequest",
        matcher: Some("*"),
        signal: SignalEvent::ApprovalAsked,
        timeout: 5,
    },
    // Codex PostToolUse also covers failed shell commands; PostToolUseFailure is not a
    // Codex event. Shell commands remain thinking: a shell is not necessarily a file edit.
    Hook {
        event: "PostToolUse",
        matcher: Some("*"),
        signal: SignalEvent::ToolEnd,
        timeout: 5,
    },
    Hook {
        event: "Stop",
        matcher: None,
        signal: SignalEvent::TurnEnd,
        timeout: 5,
    },
    Hook {
        event: "Interrupt",
        matcher: None,
        signal: SignalEvent::TurnInterrupted,
        timeout: 3,
    },
    Hook {
        event: "SessionEnd",
        matcher: None,
        signal: SignalEvent::SessionEnd,
        timeout: 3,
    },
];

pub fn settings_path() -> Result<PathBuf, Error> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("hooks.json"));
    }
    let base = directories::BaseDirs::new().ok_or(Error::NoHome)?;
    Ok(base.home_dir().join(".codex/hooks.json"))
}

pub fn install(path: &Path, hook_path: &Path) -> Result<Edit, Error> {
    json_hooks::install(path, hook_path, Harness::Codex, HOOKS)
}

pub fn inspect(path: &Path, hook_path: &Path) -> Result<Inspection, Error> {
    json_hooks::inspect(path, hook_path, Harness::Codex, HOOKS)
}

pub use super::json_hooks::uninstall;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::Saved;
    use serde_json::{Value, json};

    #[test]
    fn install_preserves_user_hooks_and_is_reversible_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        let program = Path::new("/opt/Remi Desktop/remi-hook");
        let original = json!({"description": "my hooks", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "my-notify"}]}]}});
        let original_bytes = serde_json::to_vec(&original).unwrap();
        std::fs::write(&path, &original_bytes).unwrap();
        assert_eq!(install(&path, program).unwrap().added, HOOKS.len());
        assert_eq!(
            std::fs::read(path.with_extension("json.bak")).unwrap(),
            original_bytes
        );
        assert_eq!(install(&path, program).unwrap().saved, Saved::Unchanged);
        let inspection = inspect(&path, program).unwrap();
        assert!(inspection.missing.is_empty() && inspection.unexpected.is_empty());
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            value["hooks"]["Stop"][1]["hooks"][0]["command"],
            "'/opt/Remi Desktop/remi-hook' signal turn-end --harness codex"
        );
        assert_eq!(value["hooks"]["Interrupt"][0]["hooks"][0]["timeout"], 3);
        assert!(value["hooks"].get("PostToolUseFailure").is_none());
        assert_eq!(uninstall(&path).unwrap().removed, HOOKS.len());
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value, original);
    }
}
