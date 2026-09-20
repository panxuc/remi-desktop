//! The agent harnesses `remi-hook` can be called by, and how each one tells it which session an
//! event belongs to. Which event happened always comes from the command line; this is only
//! about the details that travel with it.

mod claude;
mod codex;
mod normalize;

use std::io::{self, Read};
use std::path::PathBuf;

use crate::record::{HarnessId, SessionId};

/// An agent harness that runs `remi-hook` on its events.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Harness {
    /// Claude Code, through hooks in its settings.json.
    ClaudeCode,
    /// Codex, through lifecycle hooks in hooks.json.
    Codex,
    /// OpenCode, through a plugin.
    OpenCode,
}

impl Harness {
    /// The id written into records and used as the name of the harness's directory of session
    /// files. Pets already understand these, so they never change.
    pub fn id(self) -> HarnessId {
        let id = match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
        };
        HarnessId::new(id).expect("built-in harness ids are valid")
    }

    /// Reads what the harness passed on stdin about the session. Empty input is not an error
    /// and gives an empty [`HookInput`], so the hook can be run by hand without any.
    ///
    /// One arm per harness, each naming its own parser and its own error: what a harness sends
    /// and how it says it is that harness's business, and a new one is a new arm rather than a
    /// condition added to someone else's.
    pub fn read_input(self, stdin: impl Read) -> Result<HookInput, Error> {
        match self {
            Harness::ClaudeCode => {
                let json = read_json(stdin)?;
                claude::parse(&json).map_err(Error::ClaudeCode)
            }
            Harness::Codex => {
                let json = read_json(stdin)?;
                codex::parse(&json).map_err(Error::Codex)
            }
            // The plugin passes everything as flags. Its stdin is never read, so a plugin that
            // leaves it open cannot keep the hook waiting.
            Harness::OpenCode => Ok(HookInput::default()),
        }
    }
}

fn read_json(mut stdin: impl Read) -> Result<Vec<u8>, Error> {
    let mut json = Vec::new();
    stdin.read_to_end(&mut json).map_err(Error::Read)?;
    Ok(json)
}

/// What a harness said about the session an event belongs to. Every field is optional: flags
/// on the command line and the hook's own surroundings fill in what is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookInput {
    pub session: Option<SessionId>,
    /// Last component of the session's working directory. Never the full path.
    pub cwd: Option<String>,
    /// The harness's transcript of the session, where its title can be looked up.
    pub transcript: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading hook input: {0}")]
    Read(#[source] io::Error),
    #[error("invalid Codex hook input: {0}")]
    Codex(#[source] serde_json::Error),
    #[error("invalid Claude Code hook input: {0}")]
    ClaudeCode(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for a stdin that must not be touched.
    struct Untouchable;

    impl Read for Untouchable {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            panic!("stdin was read");
        }
    }

    #[test]
    fn opencode_never_reads_stdin() {
        assert_eq!(
            Harness::OpenCode.read_input(Untouchable).unwrap(),
            HookInput::default()
        );
    }

    #[test]
    fn claude_code_reads_its_json_from_stdin() {
        let stdin = br#"{"session_id":"s1","cwd":"/home/u/repos/remi-desktop"}"#;
        let input = Harness::ClaudeCode.read_input(&stdin[..]).unwrap();
        assert_eq!(input.session, Some(SessionId::new("s1").unwrap()));
        assert_eq!(input.cwd.as_deref(), Some("remi-desktop"));
    }

    #[test]
    fn codex_reads_its_json_from_stdin() {
        let stdin = br#"{"session_id":"thread-1","cwd":"/repos/remi","turn_id":"turn-1"}"#;
        let input = Harness::Codex.read_input(&stdin[..]).unwrap();
        assert_eq!(input.session, Some(SessionId::new("thread-1").unwrap()));
        assert_eq!(input.cwd.as_deref(), Some("remi"));
    }

    /// Each harness reports its own parse failures, so a message never names the wrong one.
    #[test]
    fn a_bad_payload_is_blamed_on_the_harness_that_sent_it() {
        assert!(matches!(
            Harness::ClaudeCode.read_input(&b"bad json"[..]),
            Err(Error::ClaudeCode(_))
        ));
        assert!(matches!(
            Harness::Codex.read_input(&b"bad json"[..]),
            Err(Error::Codex(_))
        ));
    }

    #[test]
    fn ids_are_the_documented_ones() {
        // Also proves `id` never panics.
        assert_eq!(Harness::ClaudeCode.id().as_str(), "claude-code");
        assert_eq!(Harness::Codex.id().as_str(), "codex");
        assert_eq!(Harness::OpenCode.id().as_str(), "opencode");
    }
}
