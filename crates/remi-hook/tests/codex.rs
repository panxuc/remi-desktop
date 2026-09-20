//! Exercise the installed commands through the real CLI and on-disk state store.
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

fn invoke(dir: &TempDir, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_remi-hook"))
        .args(args)
        .env("CODEX_HOME", dir.path().join("codex"))
        .env("CLAUDE_CONFIG_DIR", dir.path().join("claude"))
        .env("REMI_STATE_DIR", dir.path().join("sessions"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn setup_checks_only_codex_and_uninstall_preserves_claude_and_user_config() {
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("codex");
    std::fs::create_dir(&codex).unwrap();
    std::fs::write(codex.join("config.toml"), "model = 'my-model'\n").unwrap();
    let original =
        json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "my-notify"}]}]}});
    std::fs::write(codex.join("hooks.json"), original.to_string()).unwrap();
    success(invoke(
        &dir,
        &["setup", "--harness", "codex", "--check"],
        "",
    ));
    assert!(!dir.path().join("claude/settings.json").exists());
    assert!(
        !invoke(&dir, &["check", "--harness", "claude-code"], "")
            .status
            .success()
    );
    let installed = std::fs::read(codex.join("hooks.json")).unwrap();
    let again = success(invoke(
        &dir,
        &["setup", "--harness", "codex", "--check"],
        "",
    ));
    assert!(again.contains("already set up"));
    assert_eq!(std::fs::read(codex.join("hooks.json")).unwrap(), installed);
    success(invoke(
        &dir,
        &["setup", "--harness", "claude-code", "--check"],
        "",
    ));
    let claude = std::fs::read(dir.path().join("claude/settings.json")).unwrap();
    success(invoke(&dir, &["uninstall", "--harness", "codex"], ""));
    let restored: Value =
        serde_json::from_slice(&std::fs::read(codex.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(restored, original);
    assert_eq!(
        std::fs::read(dir.path().join("claude/settings.json")).unwrap(),
        claude
    );
    assert_eq!(
        std::fs::read_to_string(codex.join("config.toml")).unwrap(),
        "model = 'my-model'\n"
    );
    assert!(
        !invoke(&dir, &["check", "--harness", "codex"], "")
            .status
            .success()
    );
}

#[test]
fn installed_codex_events_drive_the_pet_and_clear_waiting_on_interrupt() {
    let dir = tempfile::tempdir().unwrap();
    success(invoke(
        &dir,
        &["setup", "--harness", "codex", "--check"],
        "",
    ));
    let settings: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("codex/hooks.json")).unwrap())
            .unwrap();
    for (event, group, expected) in [
        ("UserPromptSubmit", 0, "thinking"),
        ("PreToolUse", 0, "viewing"),
        ("PreToolUse", 1, "writing"),
        ("PermissionRequest", 0, "waiting_for_input"),
        ("PostToolUse", 0, "thinking"),
        ("Stop", 0, "proud"),
        ("UserPromptSubmit", 0, "thinking"),
        ("PreToolUse", 2, "waiting_for_input"),
        ("Interrupt", 0, "idle"),
        ("SessionEnd", 0, "offline"),
    ] {
        let command = settings["hooks"][event][group]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        // The executable may be quoted; everything after ` signal ` is generated CLI flags.
        let (_, flags) = command.rsplit_once(" signal ").unwrap();
        let args: Vec<_> = std::iter::once("signal")
            .chain(flags.split_whitespace())
            .collect();
        let payload = json!({"session_id":"codex-test", "cwd":"/repos/my-project", "transcript_path":null, "hook_event_name":event, "turn_id":"turn-1", "model":"test"});
        let stdout = success(invoke(&dir, &args, &payload.to_string()));
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap(),
            json!({}),
            "{event}: never alter agent control flow"
        );
        let records: Value =
            serde_json::from_str(&success(invoke(&dir, &["snapshot"], ""))).unwrap();
        if expected == "offline" {
            assert_eq!(records, json!([]));
        } else {
            assert_eq!(records[0]["state"], expected, "{event}");
            assert_eq!(records[0]["harness"], "codex");
            assert_eq!(records[0]["cwd"], "my-project");
        }
    }
}

#[test]
fn invalid_input_or_failed_writes_never_block_codex_or_escape_the_store() {
    let dir = tempfile::tempdir().unwrap();
    for input in ["not json", r#"{"session_id":"../escape"}"#, ""] {
        let out = success(invoke(
            &dir,
            &["signal", "turn-end", "--harness", "codex"],
            input,
        ));
        assert_eq!(out.trim(), "{}");
        assert_eq!(success(invoke(&dir, &["snapshot"], "")).trim(), "[]");
    }
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
    std::fs::write(
        dir.path().join("sessions/codex"),
        "blocks directory creation",
    )
    .unwrap();
    let out = success(invoke(
        &dir,
        &["signal", "turn-start", "--harness", "codex"],
        r#"{"session_id":"safe"}"#,
    ));
    assert_eq!(out.trim(), "{}");
}
