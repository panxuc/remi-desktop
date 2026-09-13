//! Claude Code hands every hook a JSON object on stdin. Each event adds its own fields, but all
//! of them carry `session_id`, `cwd` and `transcript_path`, which is all we read.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Error, HookInput};
use crate::record::SessionId;

/// The fields every Claude Code hook payload shares. The rest, such as a tool's full input and
/// output, is ignored.
#[derive(Deserialize)]
struct Payload {
    session_id: Option<SessionId>,
    cwd: Option<String>,
    transcript_path: Option<PathBuf>,
}

pub(super) fn parse(json: &[u8]) -> Result<HookInput, Error> {
    if json.iter().all(u8::is_ascii_whitespace) {
        return Ok(HookInput::default());
    }
    let payload: Payload = serde_json::from_slice(json).map_err(Error::ClaudeCode)?;
    let cwd = payload
        .cwd
        .and_then(|cwd| Some(Path::new(&cwd).file_name()?.to_string_lossy().into_owned()));
    Ok(HookInput {
        session: payload.session_id,
        cwd,
        transcript: payload.transcript_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_session_cwd_and_transcript_from_a_real_payload() {
        let json = br#"{
            "session_id": "1a0e02ad-5127-41ae-b140-139fed68bc31",
            "transcript_path": "/home/u/.claude/projects/-home-u-repos-remi-desktop/1a0e02ad-5127-41ae-b140-139fed68bc31.jsonl",
            "cwd": "/home/u/repos/remi-desktop",
            "permission_mode": "default",
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": "/home/u/repos/remi-desktop/Cargo.toml"}
        }"#;

        let input = parse(json).unwrap();

        assert_eq!(
            input.session,
            Some(SessionId::new("1a0e02ad-5127-41ae-b140-139fed68bc31").unwrap())
        );
        assert_eq!(input.cwd.as_deref(), Some("remi-desktop"));
        assert_eq!(
            input.transcript,
            Some(PathBuf::from(
                "/home/u/.claude/projects/-home-u-repos-remi-desktop/1a0e02ad-5127-41ae-b140-139fed68bc31.jsonl"
            ))
        );
    }

    #[test]
    fn missing_fields_are_none() {
        assert_eq!(
            parse(br#"{"hook_event_name":"Stop"}"#).unwrap(),
            HookInput::default()
        );
    }

    #[test]
    fn empty_input_is_no_input() {
        assert_eq!(parse(b"").unwrap(), HookInput::default());
        assert_eq!(parse(b" \n").unwrap(), HookInput::default());
    }

    #[test]
    fn keeps_only_the_last_component_of_cwd() {
        let cwd_of = |cwd: &str| {
            let json = serde_json::json!({ "cwd": cwd }).to_string();
            parse(json.as_bytes()).unwrap().cwd
        };
        assert_eq!(
            cwd_of("/home/u/repos/remi-desktop/").as_deref(),
            Some("remi-desktop")
        );
        assert_eq!(cwd_of("remi-desktop").as_deref(), Some("remi-desktop"));
        assert_eq!(cwd_of("/"), None);
    }

    #[test]
    fn rejects_junk_and_unsafe_session_ids() {
        assert!(matches!(parse(b"not json"), Err(Error::ClaudeCode(_))));
        assert!(matches!(
            parse(br#"{"session_id":"../escape"}"#),
            Err(Error::ClaudeCode(_))
        ));
    }
}
