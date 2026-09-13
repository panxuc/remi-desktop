//! What agent harnesses report, in one vocabulary shared by all of them, and the rules that
//! turn it into poses. Each harness adapter translates its own events into [`Signal`]s; this
//! module is the only place that decides which pose a signal produces, so the policy is
//! written down once rather than once per harness.

use std::time::Duration;

use crate::record::{HarnessId, SessionId, SessionRecord};
use crate::state::PetState;
use crate::store::Update;

/// How long an unchanged record may go without being rewritten. Bursts of identical signals
/// (a run of reads) then cost one write, while `ts` — which the pet orders sessions by and
/// shows as when each was last heard from — still lags the truth by at most this much.
pub const REWRITE_UNCHANGED_AFTER: Duration = Duration::from_secs(20);

/// Something an agent harness just did, named so it means the same thing for every harness.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The user submitted a prompt and the agent started on it.
    TurnStart,
    /// The agent called a tool that looks at code without changing it.
    ReadStart,
    /// The agent called a tool that changes files.
    EditStart,
    /// A tool call finished, whether or not it succeeded.
    ToolEnd,
    /// The agent is blocked on the user approving something or answering a question.
    ApprovalAsked,
    /// The user answered an approval prompt, before the tool it guarded runs. Only harnesses
    /// that report the answer send this; on the others the following [`Signal::ToolEnd`]
    /// is what clears the prompt.
    ApprovalAnswered,
    /// The agent finished its turn.
    TurnEnd,
    /// The session closed.
    SessionEnd,
}

impl Signal {
    /// Decides what happens to a session's file when this signal arrives, given the session's
    /// current record (`previous`). Pure: reading the record and applying the result are the
    /// caller's job.
    ///
    /// An ended session's file is removed. Otherwise a record is written, unless it would
    /// repeat `previous` exactly — apart from `ts` — and `previous` is younger than
    /// [`REWRITE_UNCHANGED_AFTER`].
    pub fn next_update(self, previous: Option<&SessionRecord>, context: SessionContext) -> Update {
        let current = previous.map(|record| record.state);
        let resume = previous.and_then(|record| record.resume);

        // The pose this signal moves the session to, and the pose to return to once an
        // outstanding approval prompt is answered. Every signal except a prompt clears the
        // latter: once anything else happens, the prompt is over.
        let (state, resume) = match self {
            Signal::TurnStart => (PetState::Thinking, None),
            Signal::ReadStart => (PetState::Viewing, None),
            Signal::EditStart => (PetState::Writing, None),
            // A tool that has finished is never still being read or written, even if a prompt
            // held it up: the agent is deciding what to do next. This is also what clears the
            // prompt on harnesses that never report the answer.
            Signal::ToolEnd => (PetState::Thinking, None),
            // A second prompt while one is outstanding keeps the pose from before the first.
            // Waiting is never a pose to return to, or answering could not clear it.
            Signal::ApprovalAsked => (
                PetState::WaitingForInput,
                resume
                    .or(current)
                    .filter(|state| *state != PetState::WaitingForInput),
            ),
            // The guarded tool is about to run, so go back to what it was doing.
            Signal::ApprovalAnswered => (resume.unwrap_or(PetState::Thinking), None),
            Signal::TurnEnd => (PetState::Proud, None),
            Signal::SessionEnd => {
                return Update::Remove {
                    harness: context.harness,
                    session: context.session,
                };
            }
        };

        let mut record = SessionRecord::following(
            previous,
            context.session,
            context.harness,
            state,
            context.ts,
        );
        record.resume = resume;
        record.cwd = context.cwd;
        if context.title.is_some() {
            record.title = context.title;
        }

        // Skip a record that says nothing the previous one doesn't, while the previous one is
        // recent enough to stand in for it. One from the future (the clock moved back) never is.
        let Some(previous) = previous else {
            return Update::Write(record);
        };
        let age = record.ts - previous.ts;
        let recent = (0..REWRITE_UNCHANGED_AFTER.as_secs() as i64).contains(&age);
        let unchanged = SessionRecord {
            ts: previous.ts,
            ..record.clone()
        } == *previous;
        if recent && unchanged {
            Update::Skip
        } else {
            Update::Write(record)
        }
    }
}

/// What the harness said about the session alongside a signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContext {
    pub session: SessionId,
    /// The harness the session runs in, e.g. `claude-code`.
    pub harness: HarnessId,
    /// Unix seconds when the signal was received.
    pub ts: i64,
    /// Last component of the session's working directory.
    pub cwd: Option<String>,
    /// The session's title, if the harness reported one this time. When `None`, a title
    /// already in the previous record is kept.
    pub title: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use PetState::*;
    use Signal::*;

    const T0: i64 = 1_757_150_400;

    fn context(ts: i64) -> SessionContext {
        SessionContext {
            session: SessionId::new("s1").unwrap(),
            harness: HarnessId::new("claude-code").unwrap(),
            ts,
            cwd: Some("remi-desktop".into()),
            title: None,
        }
    }

    fn record(state: PetState, resume: Option<PetState>) -> SessionRecord {
        let mut record = SessionRecord::new(
            SessionId::new("s1").unwrap(),
            HarnessId::new("claude-code").unwrap(),
            state,
            T0,
        );
        record.resume = resume;
        record.cwd = Some("remi-desktop".into());
        record.started = Some(T0);
        record
    }

    fn written(update: Update) -> SessionRecord {
        match update {
            Update::Write(record) => record,
            other => panic!("expected a write, got {other:?}"),
        }
    }

    /// Feeds `signals` one second apart to a session with no record, applying each update the
    /// way the hook would, and returns the pose after each (`Offline` once the file is gone).
    fn poses(signals: &[Signal]) -> Vec<PetState> {
        let mut current: Option<SessionRecord> = None;
        let mut poses = Vec::new();
        for (i, &signal) in signals.iter().enumerate() {
            match signal.next_update(current.as_ref(), context(T0 + i as i64)) {
                Update::Write(record) => current = Some(record),
                Update::Skip => {}
                Update::Remove { .. } => current = None,
            }
            poses.push(current.as_ref().map_or(Offline, |record| record.state));
        }
        poses
    }

    #[test]
    fn each_signal_moves_to_its_pose() {
        let waiting_from_writing = Some((WaitingForInput, Some(Writing)));
        let cases = [
            // (previous state and resume, signal, expected state and resume)
            (None, TurnStart, (Thinking, None)),
            (None, ReadStart, (Viewing, None)),
            (None, EditStart, (Writing, None)),
            (None, ToolEnd, (Thinking, None)),
            (None, ApprovalAsked, (WaitingForInput, None)),
            (None, ApprovalAnswered, (Thinking, None)),
            (None, TurnEnd, (Proud, None)),
            (
                Some((Viewing, None)),
                ApprovalAsked,
                (WaitingForInput, Some(Viewing)),
            ),
            (
                waiting_from_writing,
                ApprovalAsked,
                (WaitingForInput, Some(Writing)),
            ),
            (
                Some((WaitingForInput, None)),
                ApprovalAsked,
                (WaitingForInput, None),
            ),
            (waiting_from_writing, ApprovalAnswered, (Writing, None)),
            (waiting_from_writing, ToolEnd, (Thinking, None)),
            (waiting_from_writing, TurnStart, (Thinking, None)),
            (waiting_from_writing, ReadStart, (Viewing, None)),
            (waiting_from_writing, TurnEnd, (Proud, None)),
        ];
        for (previous, signal, expected) in cases {
            let previous = previous.map(|(state, resume)| record(state, resume));
            let next = written(signal.next_update(previous.as_ref(), context(T0 + 60)));
            assert_eq!(
                (next.state, next.resume),
                expected,
                "{signal:?} after {previous:?}"
            );
        }
    }

    #[test]
    fn an_answered_prompt_returns_to_writing_then_the_tool_ends() {
        // A harness that reports the answer before the tool runs.
        assert_eq!(
            poses(&[
                TurnStart,
                EditStart,
                ApprovalAsked,
                ApprovalAnswered,
                ToolEnd,
                TurnEnd
            ]),
            [Thinking, Writing, WaitingForInput, Writing, Thinking, Proud]
        );
    }

    #[test]
    fn a_tool_ending_clears_a_prompt_that_was_never_answered() {
        // A harness with no answer event: tool completion is the only sign of approval.
        assert_eq!(
            poses(&[TurnStart, EditStart, ApprovalAsked, ToolEnd, TurnEnd]),
            [Thinking, Writing, WaitingForInput, Thinking, Proud]
        );
    }

    #[test]
    fn a_second_prompt_keeps_the_pose_from_before_the_first() {
        assert_eq!(
            poses(&[EditStart, ApprovalAsked, ApprovalAsked, ApprovalAnswered]),
            [Writing, WaitingForInput, WaitingForInput, Writing]
        );
    }

    #[test]
    fn an_ended_session_is_removed() {
        let previous = record(Thinking, None);
        assert_eq!(
            SessionEnd.next_update(Some(&previous), context(T0 + 60)),
            Update::Remove {
                harness: HarnessId::new("claude-code").unwrap(),
                session: SessionId::new("s1").unwrap(),
            }
        );
        assert_eq!(poses(&[TurnStart, SessionEnd]), [Thinking, Offline]);
    }

    #[test]
    fn a_first_record_carries_the_context() {
        let mut context = context(T0);
        context.title = Some("Creating bin from crate libs".into());

        let next = written(TurnStart.next_update(None, context));

        assert_eq!(next.session.as_str(), "s1");
        assert_eq!(next.harness.as_str(), "claude-code");
        assert_eq!(next.ts, T0);
        assert_eq!(next.started, Some(T0));
        assert_eq!(next.cwd.as_deref(), Some("remi-desktop"));
        assert_eq!(next.title.as_deref(), Some("Creating bin from crate libs"));
    }

    #[test]
    fn a_known_title_is_kept_until_a_new_one_arrives() {
        let mut previous = record(Thinking, None);
        previous.title = Some("old".into());

        let kept = written(ReadStart.next_update(Some(&previous), context(T0 + 1)));
        assert_eq!(kept.title.as_deref(), Some("old"));

        let mut renamed = context(T0 + 1);
        renamed.title = Some("new".into());
        let replaced = written(ReadStart.next_update(Some(&previous), renamed));
        assert_eq!(replaced.title.as_deref(), Some("new"));
    }

    #[test]
    fn an_unchanged_record_is_skipped_while_recent() {
        let previous = record(Viewing, None);
        let limit = REWRITE_UNCHANGED_AFTER.as_secs() as i64;

        assert_eq!(
            ReadStart.next_update(Some(&previous), context(T0)),
            Update::Skip
        );
        assert_eq!(
            ReadStart.next_update(Some(&previous), context(T0 + limit - 1)),
            Update::Skip
        );
        assert_eq!(
            written(ReadStart.next_update(Some(&previous), context(T0 + limit))).ts,
            T0 + limit
        );
    }

    #[test]
    fn any_change_besides_ts_is_written() {
        let previous = record(Viewing, None);

        let mut moved = context(T0 + 1);
        moved.cwd = Some("elsewhere".into());
        assert!(matches!(
            ReadStart.next_update(Some(&previous), moved),
            Update::Write(_)
        ));

        let mut titled = context(T0 + 1);
        titled.title = Some("a title".into());
        assert!(matches!(
            ReadStart.next_update(Some(&previous), titled),
            Update::Write(_)
        ));

        let mut other_harness = context(T0 + 1);
        other_harness.harness = HarnessId::new("opencode").unwrap();
        assert!(matches!(
            ReadStart.next_update(Some(&previous), other_harness),
            Update::Write(_)
        ));
    }

    #[test]
    fn a_record_from_the_future_is_rewritten() {
        let previous = record(Viewing, None);
        assert!(matches!(
            ReadStart.next_update(Some(&previous), context(T0 - 5)),
            Update::Write(_)
        ));
    }
}
