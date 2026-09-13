use std::fmt;

use serde::{Deserialize, Serialize};

use crate::state::PetState;

/// The record format version this build writes and understands.
pub const VERSION: u8 = 1;

/// One session's current state. A state file's body, a line of `remi-hook watch` output, and
/// an MQTT payload are all exactly this, as JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Format version. A reader rejects a version it doesn't know rather than guess at it.
    pub v: u8,
    pub session: SessionId,
    /// Which harness runs the session, e.g. `claude-code`. A string rather than an enum, so a
    /// pet still reads records from a harness newer than itself.
    pub harness: String,
    pub state: PetState,
    /// Unix seconds on the writer's clock. Orders records for the same session; never used to
    /// judge staleness, since the writer's clock may be skewed against the pet's.
    pub ts: i64,
    /// The session's own title, when the harness has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Last component of the session's working directory. Never a full path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// `ts` of the first record written for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<i64>,
    /// Writer-private: the pose to return to once an approval prompt clears.
    #[serde(rename = "_resume", default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<PetState>,
}

impl SessionRecord {
    /// A record at the current format version, with every optional field empty.
    pub fn new(session: SessionId, harness: impl Into<String>, state: PetState, ts: i64) -> Self {
        Self {
            v: VERSION,
            session,
            harness: harness.into(),
            state,
            ts,
            title: None,
            cwd: None,
            started: None,
            resume: None,
        }
    }

    /// The record that replaces `previous` for the same session. What belongs to the session
    /// rather than to one moment — when it started, and its title once known — carries over;
    /// everything else starts empty.
    pub fn following(
        previous: Option<&SessionRecord>,
        session: SessionId,
        harness: impl Into<String>,
        state: PetState,
        ts: i64,
    ) -> Self {
        let mut record = Self::new(session, harness, state, ts);
        // A previous record without `started` still proves the session existed by its `ts`.
        record.started = Some(previous.map_or(ts, |prev| prev.started.unwrap_or(prev.ts)));
        record.title = previous.and_then(|prev| prev.title.clone());
        record
    }

    /// Parses a record, rejecting any format version other than [`VERSION`]. Unknown fields
    /// are ignored, so older readers keep working when fields are added.
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        #[derive(Deserialize)]
        struct Version {
            v: u8,
        }

        let Version { v } = serde_json::from_slice(bytes).map_err(Error::Json)?;
        if v != VERSION {
            return Err(Error::UnsupportedVersion(v));
        }
        serde_json::from_slice(bytes).map_err(Error::Json)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a SessionRecord always serializes")
    }
}

/// A harness's session id, checked to be safe as a filename: 1 to [`SessionId::MAX_LEN`]
/// ASCII letters, digits, `-` or `_`. Claude Code's UUIDs and OpenCode's `ses_…` ids fit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionId(String);

impl SessionId {
    pub const MAX_LEN: usize = 128;

    pub fn new(id: impl Into<String>) -> Result<Self, Error> {
        let id = id.into();
        let valid = !id.is_empty()
            && id.len() <= Self::MAX_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if valid {
            Ok(Self(id))
        } else {
            Err(Error::InvalidSessionId(id))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SessionId {
    type Error = Error;

    fn try_from(id: String) -> Result<Self, Error> {
        Self::new(id)
    }
}

impl From<SessionId> for String {
    fn from(id: SessionId) -> Self {
        id.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid session record: {0}")]
    Json(#[source] serde_json::Error),
    #[error("unsupported session record version {0} (this build reads version {current})", current = VERSION)]
    UnsupportedVersion(u8),
    #[error(
        "invalid session id {0:?}: use 1 to {max} ASCII letters, digits, '-' or '_'",
        max = SessionId::MAX_LEN
    )]
    InvalidSessionId(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_record() -> SessionRecord {
        SessionRecord {
            v: VERSION,
            session: SessionId::new("1a0e02ad-5127-41ae-b140-139fed68bc31").unwrap(),
            harness: "claude-code".into(),
            state: PetState::WaitingForInput,
            ts: 1_757_150_400,
            title: Some("Creating bin from crate libs".into()),
            cwd: Some("remi-desktop".into()),
            started: Some(1_757_150_100),
            resume: Some(PetState::Writing),
        }
    }

    #[test]
    fn round_trips() {
        let record = full_record();
        assert_eq!(SessionRecord::from_json(&record.to_json()).unwrap(), record);
    }

    #[test]
    fn uses_the_documented_field_names() {
        let json = String::from_utf8(full_record().to_json()).unwrap();
        assert!(json.contains(r#""state":"waiting_for_input""#), "{json}");
        assert!(json.contains(r#""_resume":"writing""#), "{json}");
    }

    #[test]
    fn omits_empty_optional_fields() {
        let record = SessionRecord::new(
            SessionId::new("s1").unwrap(),
            "opencode",
            PetState::Thinking,
            1,
        );
        let json = String::from_utf8(record.to_json()).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"session":"s1","harness":"opencode","state":"thinking","ts":1}"#
        );
    }

    #[test]
    fn ignores_unknown_fields() {
        let json = br#"{"v":1,"session":"s1","harness":"claude-code","state":"proud","ts":5,"future":[1,2]}"#;
        let record = SessionRecord::from_json(json).unwrap();
        assert_eq!(record.state, PetState::Proud);
    }

    #[test]
    fn rejects_unknown_versions() {
        let json = br#"{"v":2,"session":"s1","harness":"claude-code","state":"proud","ts":5}"#;
        assert!(matches!(
            SessionRecord::from_json(json),
            Err(Error::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn rejects_unsafe_session_ids_when_parsing() {
        let json =
            br#"{"v":1,"session":"../escape","harness":"claude-code","state":"proud","ts":5}"#;
        assert!(matches!(
            SessionRecord::from_json(json),
            Err(Error::Json(_))
        ));
    }

    #[test]
    fn a_first_record_starts_now() {
        let id = SessionId::new("s1").unwrap();
        let record = SessionRecord::following(None, id, "claude-code", PetState::Thinking, 500);
        assert_eq!(record.started, Some(500));
        assert_eq!(record.title, None);
    }

    #[test]
    fn following_carries_over_start_and_title_only() {
        let previous = full_record();
        let record = SessionRecord::following(
            Some(&previous),
            previous.session.clone(),
            "claude-code",
            PetState::Proud,
            1_757_150_999,
        );
        assert_eq!(record.started, previous.started);
        assert_eq!(record.title, previous.title);
        assert_eq!(record.state, PetState::Proud);
        assert_eq!(record.ts, 1_757_150_999);
        assert_eq!(record.cwd, None);
        assert_eq!(record.resume, None);
    }

    #[test]
    fn following_a_record_without_start_uses_its_timestamp() {
        let mut previous = full_record();
        previous.started = None;
        let record = SessionRecord::following(
            Some(&previous),
            previous.session.clone(),
            "claude-code",
            PetState::Thinking,
            previous.ts + 60,
        );
        assert_eq!(record.started, Some(previous.ts));
    }

    #[test]
    fn validates_session_ids() {
        for ok in [
            "1a0e02ad-5127-41ae-b140-139fed68bc31",
            "ses_1539aaa93ffe2d2KbOo4LWJaIl",
            "manual",
        ] {
            assert!(SessionId::new(ok).is_ok(), "{ok}");
        }
        let too_long = "a".repeat(SessionId::MAX_LEN + 1);
        for bad in ["", "..", "a/b", "a\\b", "a.json", "a b", too_long.as_str()] {
            assert!(SessionId::new(bad).is_err(), "{bad}");
        }
    }
}
