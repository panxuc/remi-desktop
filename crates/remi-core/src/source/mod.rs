//! How the pet reads session records: one source per configured connection, each reporting
//! what it hears as [`SessionUpdate`]s for the registry. Each transport has its own module
//! here.

pub mod local;

use std::fmt;

use crate::record::{HarnessId, SessionId, SessionRecord};

/// The name of a configured connection, e.g. `local`, or `plume` for an ssh host. The user
/// picks it in the pet's config, and the menu shows it as the machine's name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(String);

impl ConnectionId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Something one connection heard. The connection itself travels beside the update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionUpdate {
    /// A session's current record, either new or replacing an earlier one.
    Upsert(SessionRecord),
    /// A session ended: its file was deleted, or its retained message cleared.
    Removed {
        harness: HarnessId,
        session: SessionId,
    },
    /// Every live session on the connection, replacing whatever was known about it before.
    /// Sent when a source attaches or re-attaches, so sessions that ended while it was away
    /// disappear.
    Snapshot(Vec<SessionRecord>),
    /// The connection is working.
    ConnectionUp,
    /// The connection failed and the source is retrying. What it heard before is kept.
    ConnectionLost { reason: String },
}
