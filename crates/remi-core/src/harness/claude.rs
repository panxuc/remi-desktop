//! Claude Code hands every hook a JSON object on stdin. Each event adds its own fields — a
//! tool's name, its input, the permission mode — but all of them carry `session_id`, `cwd` and
//! `transcript_path`, which is all we read.

use std::path::PathBuf;

use serde::Deserialize;

use super::{HookInput, normalize};
use crate::record::SessionId;

/// Claude Code decides on a hook's exit code and reads stdout only from a hook that opts into
/// answering. Saying nothing is how a hook stays out of the way.
pub(super) const REPLY: Option<&str> = None;

/// The fields every Claude Code hook payload shares. The rest, such as a tool's input and
/// output, is ignored.
#[derive(Deserialize)]
struct Payload {
    session_id: Option<SessionId>,
    cwd: Option<String>,
    transcript_path: Option<PathBuf>,
}

pub(super) fn parse(json: &[u8]) -> Result<HookInput, serde_json::Error> {
    if normalize::is_blank(json) {
        return Ok(HookInput::default());
    }
    let payload: Payload = serde_json::from_slice(json)?;
    Ok(HookInput {
        session: payload.session_id,
        cwd: payload.cwd.as_deref().and_then(normalize::cwd_name),
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
    fn rejects_junk_and_unsafe_session_ids() {
        assert!(parse(b"not json").is_err());
        assert!(parse(br#"{"session_id":"../escape"}"#).is_err());
    }
}
