//! Codex hands every lifecycle hook a JSON object on stdin. Each event adds its own fields —
//! `turn_id`, a tool's name and arguments — but all of them carry `session_id`, `cwd` and
//! `transcript_path`, which is all we read. `transcript_path` is often null.
//!
//! These are the same three names Claude Code uses (`claude.rs`), and the two are parsed apart
//! anyway: they are separate contracts with separate owners, and either can rename a field
//! without asking the other.

use std::path::PathBuf;

use serde::Deserialize;

use super::{HookInput, normalize};
use crate::record::SessionId;

/// Codex reads a hook's stdout as that hook's answer and parses it as JSON, so writing nothing
/// is a parse error inside the agent rather than silence. `{}` is the answer that decides
/// nothing, which is the only answer a pet is ever allowed to give.
pub(super) const REPLY: Option<&str> = Some("{}");

/// The fields every Codex hook payload shares. The rest is ignored.
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
    fn reads_session_and_cwd_from_a_real_payload() {
        let json = br#"{
            "session_id": "thread-1",
            "cwd": "/home/u/repos/remi-desktop",
            "transcript_path": null,
            "turn_id": "turn-1",
            "hook_event_name": "PreToolUse"
        }"#;

        let input = parse(json).unwrap();

        assert_eq!(input.session, Some(SessionId::new("thread-1").unwrap()));
        assert_eq!(input.cwd.as_deref(), Some("remi-desktop"));
        assert_eq!(input.transcript, None);
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
