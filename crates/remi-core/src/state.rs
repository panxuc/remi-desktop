use serde::{Deserialize, Serialize};

/// What Remi is shown doing for a session.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PetState {
    /// Working out what to do next.
    Thinking,
    /// Reading code.
    Viewing,
    /// Changing files.
    Writing,
    /// Writing its reply to the user.
    Replying,
    /// Blocked on the user approving something or answering a question.
    WaitingForInput,
    /// Just finished a turn; the pet fades this to idle after a few seconds.
    Proud,
    /// Between turns: what `Proud` fades into. Never written to a record, since the pet derives
    /// it by timing `Proud` on its own clock. It is in this enum so the renderer has a single
    /// input type.
    Idle,
    /// The session has ended.
    Offline,
}
