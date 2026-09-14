//! One remote machine's sessions, over ssh.
//!
//! The pet runs `remi-hook watch` on the remote and reads its stdout: one JSON array per line,
//! the whole listing each time, printed once at the start and again whenever anything changes.
//! The remote half is the same [`StateDirWatch`](super::local::StateDirWatch) the `local` source
//! runs here, so a session on another machine reaches the registry in exactly the shape one on
//! this machine does. There is no delta format and no protocol to version: a session that ended
//! is simply missing from the next line, and each record carries its own format version.
//!
//! We spawn the system `ssh` binary rather than speaking ssh ourselves, specifically so the
//! user's own `~/.ssh/config` applies for free — `ProxyJump`, `IdentityFile`, `Port`,
//! `known_hosts`, agent forwarding, all of it. A Rust ssh client would mean reimplementing
//! `ssh_config` parsing, and would be the thing that forced this tool to be configured
//! separately from the ssh the user already has working.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use crate::record::{self, SessionRecord};
use crate::source::SessionUpdate;

/// What is run on the remote.
///
/// **An absolute path, never `$PATH`.** `~/.local/bin` is frequently missing from a
/// non-interactive ssh shell's `PATH`, and whether a login shell runs at all under `sshd` is
/// distro-dependent. Trusting `PATH` produces a host that is installed but silent, which is the
/// hardest failure here to recognise — it looks exactly like a machine where nothing is
/// happening. The remote's shell expands `$HOME`.
const REMOTE_COMMAND: &str = "$HOME/.local/bin/remi-hook watch";

/// How long to wait before the first retry, and the ceiling the wait doubles up to. A host that
/// is simply switched off is then asked about twice a minute, which costs nothing and means it
/// comes back on its own within half a minute of being switched on.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// How many of the child's last stderr lines are kept to explain a failure. Two, because ssh's
/// own message is usually one line and the remote shell's is another.
const REASON_LINES: usize = 2;

/// How long the stderr drain gets to finish after the child has gone.
///
/// Bounded rather than joined: the user's `~/.ssh/config` is in charge here, and a
/// `ControlMaster auto` in it leaves a background ssh holding the same stderr open. Joining
/// would then wait for *that* process, which outlives this connection on purpose.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

/// A shell's exit status for a command it could not find. Against a host nobody has installed to
/// yet this is the first thing that happens, and it deserves plain language.
const NOT_FOUND: i32 = 127;

/// The ssh invocation for one host.
///
/// Only the options that this pipe needs are set. Everything else is deliberately left to the
/// user's own config — and nothing is ever written to it.
pub fn ssh_command(host: &str) -> Command {
    let mut command = Command::new("ssh");
    command.args([
        // No pty. This is a pipe, and a pty would echo, translate newlines, and leave
        // `remi-hook watch` reading a terminal rather than a stdin that can close.
        "-T",
        // Fail visibly rather than hang on a passphrase prompt nobody can see.
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        // Keepalives are not optional here. Without them a host that drops off the network
        // mid-connection leaves us blocked in a read the kernel will never complete: the pet
        // would keep showing that machine's last pose for ever and never retry, which reads
        // exactly like "nothing is happening there" — the one thing this project must not get
        // wrong. Three missed probes at 15 s is a dead connection inside a minute.
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        // Never become a multiplexing master. With `ControlMaster auto` in the user's config,
        // an ssh that finds no master forks a *background* one — which inherits this pipe and
        // outlives the connection, so killing our own child would leave the pipe open and the
        // reader blocked on it for ever. Refusing to be a master does not stop us using one that
        // already exists, and there is only ever one connection per host here anyway.
        "-o",
        "ControlMaster=no",
        host,
        REMOTE_COMMAND,
    ]);
    command
}

/// Watches one connection until [`Stop::stop`] is called, reconnecting on its own with a
/// lengthening wait in between.
///
/// `launch` builds the child afresh for each attempt — [`ssh_command`] in the app, something
/// local in tests. `report` is called from this thread for each update, so it should hand the
/// update on rather than do work in it.
pub fn watch(launch: impl Fn() -> Command, stop: &Stop, mut report: impl FnMut(SessionUpdate)) {
    let mut retry_in = FIRST_RETRY;
    while !stop.is_stopped() {
        report(SessionUpdate::Connecting);
        let attempt = attempt(&launch, stop, &mut report);

        // A stop is not a failure and has nothing to report: whoever called it is about to say
        // what happened to the connection.
        if stop.is_stopped() {
            return;
        }
        report(SessionUpdate::ConnectionLost {
            reason: attempt.reason,
        });

        // A connection that worked and then dropped starts over at the short wait. Whatever went
        // wrong is news, not a host that has been unreachable all along.
        if attempt.heard {
            retry_in = FIRST_RETRY;
        }
        if !stop.wait(retry_in) {
            return;
        }
        retry_in = (retry_in * 2).min(LONGEST_RETRY);
    }
}

/// What became of one attempt: whether the remote ever answered, and what to tell the user.
struct Attempt {
    heard: bool,
    reason: String,
}

fn attempt(
    launch: &impl Fn() -> Command,
    stop: &Stop,
    report: &mut impl FnMut(SessionUpdate),
) -> Attempt {
    let mut command = launch();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        // No ssh on this machine, or no permission to run it. Retrying will not fix it, but
        // saying so every 30 s is cheaper than a special case.
        Err(err) => {
            return Attempt {
                heard: false,
                reason: format!("cannot run ssh: {err}"),
            };
        }
    };
    let stdout = child.stdout.take().expect("stdout was piped");
    let tail = Tail::draining(child.stderr.take().expect("stderr was piped"));

    // ⚠️ The child's stdin is piped and then left alone. `remi-hook watch` exits when its stdin
    // closes, so this end going away is what reaps the remote process — which is why the pipe
    // must stay open for exactly as long as the connection, and must not be dropped early.
    if !stop.adopt(child) {
        return Attempt {
            heard: false,
            reason: String::new(),
        };
    }

    let mut heard = false;
    for line in BufReader::new(stdout).lines() {
        let problem = match line {
            Err(err) => format!("lost the connection: {err}"),
            Ok(line) => match parse(&line) {
                Ok(records) => {
                    heard = true;
                    report(SessionUpdate::Snapshot(records));
                    continue;
                }
                Err(problem) => problem,
            },
        };
        return finish(stop, &tail, heard, Some(problem));
    }
    finish(stop, &tail, heard, None)
}

/// Reaps the child and works out what to tell the user. `problem` is set when this end gave up on
/// the connection, in which case the child is still running and has to be killed.
fn finish(stop: &Stop, tail: &Tail, heard: bool, problem: Option<String>) -> Attempt {
    let status = stop.take_child().and_then(|mut child| {
        if problem.is_some() {
            let _ = child.kill();
        }
        child.wait().ok()
    });
    let reason = problem.unwrap_or_else(|| explain(status, tail));
    Attempt { heard, reason }
}

/// Why the connection ended, in the words most likely to tell the user what to do about it.
///
/// ssh's own stderr is preferred over its exit status because it is the part that names the
/// cause — `Permission denied`, `Connection refused`, `Host key verification failed` — where the
/// status is 255 for every one of them.
fn explain(status: Option<ExitStatus>, tail: &Tail) -> String {
    if status.and_then(|status| status.code()) == Some(NOT_FOUND) {
        return "remi-hook is not installed there".to_owned();
    }
    match (tail.lines(), status.and_then(|status| status.code())) {
        (Some(said), _) => said,
        (None, Some(0)) => "the remote stopped watching".to_owned(),
        (None, Some(code)) => format!("ssh exited with status {code}"),
        (None, None) => "the connection ended".to_owned(),
    }
}

/// One line of `remi-hook watch` output: a JSON array of records.
///
/// Each record is read on its own, so one this build cannot make sense of never throws away the
/// rest of the line. A record from a *newer* format version is the exception: it means the host
/// is running a `remi-hook` this pet does not understand, and guessing at records is worse than
/// saying so, so it ends the connection with something the user can act on.
fn parse(line: &str) -> Result<Vec<SessionRecord>, String> {
    let values: Vec<serde_json::Value> =
        serde_json::from_str(line).map_err(|err| format!("unreadable output: {err}"))?;

    let mut records = Vec::with_capacity(values.len());
    for value in values {
        let json = serde_json::to_vec(&value).expect("a value just parsed serializes again");
        match SessionRecord::from_json(&json) {
            Ok(record) => records.push(record),
            Err(record::Error::UnsupportedVersion(found)) => {
                return Err(format!(
                    "remi-hook there writes v{found}, this Remi reads v{} — update it",
                    record::VERSION
                ));
            }
            Err(err) => tracing::warn!("ignoring a record: {err}"),
        }
    }
    Ok(records)
}

/// Ends a [`watch`] from another thread.
///
/// Stopping has to reach a thread that is blocked reading the child's output, which no flag can
/// do on its own — so it kills the child, and the read ends at the closed pipe. The same call
/// cuts short a wait between retries.
#[derive(Clone, Default)]
pub struct Stop(Arc<Inner>);

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    stopped: bool,
    /// The running child, while there is one, so that stopping can reach it.
    child: Option<Child>,
}

impl Stop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ends the watch. Returns once the child has been killed and reaped, so a caller may start
    /// a new connection to the same host straight after.
    pub fn stop(&self) {
        let mut state = self.lock();
        state.stopped = true;
        let child = state.child.take();
        drop(state);
        self.0.changed.notify_all();

        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.lock().stopped
    }

    /// Hands the running child over, so [`Stop::stop`] can reach it. `false` means the watch was
    /// stopped while the child was starting, and the caller should give up: the child is already
    /// killed.
    fn adopt(&self, mut child: Child) -> bool {
        let mut state = self.lock();
        if state.stopped {
            drop(state);
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        state.child = Some(child);
        true
    }

    /// Takes the child back, to reap it after its output has ended.
    fn take_child(&self) -> Option<Child> {
        self.lock().child.take()
    }

    /// Waits out the retry gap. `false` means the watch was stopped instead.
    fn wait(&self, how_long: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(state, how_long, |state| !state.stopped)
            .expect("ssh watch mutex poisoned");
        !state.stopped
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().expect("ssh watch mutex poisoned")
    }
}

/// The child's last few stderr lines, kept to explain a failure.
///
/// ⚠️ Drained continuously rather than read at the end. ssh writes banners, MOTDs and warnings
/// there, and a pipe nobody reads fills up and blocks the process writing to it — so a remote
/// with a chatty login would hang the connection instead of failing it.
struct Tail {
    lines: Arc<Mutex<VecDeque<String>>>,
    /// Closed by the draining thread when it ends, which is how [`Tail::lines`] waits for the
    /// last of the output without joining anything.
    drained: Receiver<()>,
}

impl Tail {
    fn draining(stderr: ChildStderr) -> Self {
        let lines = Arc::new(Mutex::new(VecDeque::new()));
        let (alive, drained) = mpsc::channel();
        let collecting = lines.clone();
        let started = thread::Builder::new()
            .name("remi-ssh-stderr".into())
            .spawn(move || {
                // Never sent on; the drop at the end of this thread is the whole signal.
                let _alive = alive;
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let line = line.trim().to_owned();
                    if line.is_empty() {
                        continue;
                    }
                    tracing::debug!("ssh: {line}");
                    let mut lines = collecting.lock().expect("ssh stderr mutex poisoned");
                    if lines.len() == REASON_LINES {
                        lines.pop_front();
                    }
                    lines.push_back(line);
                }
            });
        if let Err(err) = &started {
            // Without the drain the child could block on a full pipe, so this is worth a line.
            tracing::warn!("not collecting ssh's stderr: {err}");
        }
        Self { lines, drained }
    }

    /// What the child said, once it has finished saying it.
    fn lines(&self) -> Option<String> {
        match self.drained.recv_timeout(DRAIN_GRACE) {
            // Nothing is ever sent, so this cannot happen; a timeout means something still holds
            // the pipe open, and whatever has arrived so far is the best answer available.
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {}
        }
        let lines = self.lines.lock().expect("ssh stderr mutex poisoned");
        (!lines.is_empty()).then(|| lines.iter().cloned().collect::<Vec<_>>().join("; "))
    }
}

/// Tests drive [`watch`] against a local shell rather than a real remote: what is interesting
/// here is the lifecycle — lines in, failures explained, retries, stopping — and none of it is
/// about ssh itself. Unix-only, for `sh`; the Windows build gets its first exercise at M6.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::record::{HarnessId, SessionId};
    use crate::state::PetState;

    /// Far longer than any of these scripts need, so a loaded machine cannot fail a test, while
    /// a watch that never ends still can.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// A child that runs `script` under `sh` rather than connecting to anything.
    fn shell(script: String) -> impl Fn() -> Command {
        move || {
            let mut command = Command::new("sh");
            command.args(["-c", &script]);
            command
        }
    }

    /// Runs a watch on a thread, stopping it as soon as `enough` holds of what it has reported,
    /// and returns all of it.
    ///
    /// Stopping from the report rather than on a timer is what keeps these tests quick and
    /// deterministic: a watch reconnects for ever by design, so something has to end it, and
    /// "when the interesting thing has happened" beats "after long enough".
    fn run(
        launch: impl Fn() -> Command + Send + 'static,
        enough: impl Fn(&[SessionUpdate]) -> bool + Send + 'static,
    ) -> Vec<SessionUpdate> {
        let stop = Stop::new();
        let stopping = stop.clone();
        let (done, updates) = mpsc::channel();
        thread::spawn(move || {
            let mut seen = Vec::new();
            watch(launch, &stop, |update| {
                seen.push(update);
                if enough(&seen) {
                    stop.stop();
                }
            });
            let _ = done.send(seen);
        });
        let seen = updates
            .recv_timeout(PATIENCE)
            .expect("the watch never ended");
        assert!(stopping.is_stopped(), "the watch ended by itself: {seen:?}");
        seen
    }

    /// The line `remi-hook watch` prints for one session in `state`.
    fn line(session: &str, state: &str) -> String {
        format!(
            r#"[{{"v":1,"session":"{session}","harness":"claude-code","state":"{state}","ts":1757150400}}]"#
        )
    }

    fn snapshots(updates: &[SessionUpdate]) -> Vec<Vec<SessionRecord>> {
        updates
            .iter()
            .filter_map(|update| match update {
                SessionUpdate::Snapshot(records) => Some(records.clone()),
                _ => None,
            })
            .collect()
    }

    fn is_lost(update: &SessionUpdate) -> bool {
        matches!(update, SessionUpdate::ConnectionLost { .. })
    }

    /// Runs until the connection is reported lost once, and returns the reason given.
    fn reason_from(script: &str) -> String {
        let updates = run(shell(script.to_owned()), |seen| seen.iter().any(is_lost));
        updates
            .iter()
            .find_map(|update| match update {
                SessionUpdate::ConnectionLost { reason } => Some(reason.clone()),
                _ => None,
            })
            .expect("nothing reported the connection lost")
    }

    #[test]
    fn every_line_of_output_is_one_snapshot() {
        let script = format!(
            "echo '{}'; echo '{}'; exec sleep 30",
            line("first", "thinking"),
            line("second", "waiting_for_input")
        );

        let updates = run(shell(script), |seen| snapshots(seen).len() == 2);

        let snapshots = snapshots(&updates);
        assert_eq!(snapshots[0][0].session, SessionId::new("first").unwrap());
        assert_eq!(snapshots[0][0].state, PetState::Thinking);
        assert_eq!(snapshots[1][0].state, PetState::WaitingForInput);
        assert_eq!(
            snapshots[1][0].harness,
            HarnessId::new("claude-code").unwrap()
        );
    }

    #[test]
    fn the_first_thing_reported_is_that_it_is_connecting() {
        let updates = run(shell("echo '[]'; exec sleep 30".to_owned()), |seen| {
            !snapshots(seen).is_empty()
        });

        assert!(
            matches!(updates[0], SessionUpdate::Connecting),
            "{updates:?}"
        );
    }

    #[test]
    fn an_empty_listing_is_a_connection_that_works_with_nothing_to_show() {
        let updates = run(shell("echo '[]'; exec sleep 30".to_owned()), |seen| {
            !snapshots(seen).is_empty()
        });

        assert_eq!(snapshots(&updates), vec![Vec::new()]);
    }

    #[test]
    fn a_record_this_build_cannot_read_does_not_lose_the_rest_of_the_line() {
        // An id no filename could hold, beside a perfectly good record.
        let script = r#"echo '[{"v":1,"session":"../escape","harness":"claude-code","state":"idle","ts":1},{"v":1,"session":"fine","harness":"claude-code","state":"idle","ts":2}]'; exec sleep 30"#;

        let updates = run(shell(script.to_owned()), |seen| !snapshots(seen).is_empty());

        let snapshots = snapshots(&updates);
        assert_eq!(snapshots[0].len(), 1);
        assert_eq!(snapshots[0][0].session, SessionId::new("fine").unwrap());
    }

    #[test]
    fn a_newer_record_format_ends_the_connection_instead_of_being_guessed_at() {
        let script = r#"echo '[{"v":99,"session":"s","harness":"claude-code","state":"idle","ts":1}]'; exec sleep 30"#;

        let reason = reason_from(script);

        assert!(reason.contains("v99"), "{reason}");
        assert!(reason.contains("update it"), "{reason}");
    }

    #[test]
    fn a_remote_without_remi_hook_says_so_rather_than_quoting_a_shell() {
        let reason = reason_from("echo 'sh: 1: remi-hook: not found' >&2; exit 127");

        assert_eq!(reason, "remi-hook is not installed there");
    }

    #[test]
    fn ssh_own_words_are_what_the_user_is_told() {
        let reason =
            reason_from("echo 'someone@host: Permission denied (publickey).' >&2; exit 255");

        assert_eq!(reason, "someone@host: Permission denied (publickey).");
    }

    #[test]
    fn only_the_last_of_a_chatty_login_is_kept() {
        let reason =
            reason_from("echo one >&2; echo two >&2; echo 'the useful part' >&2; exit 255");

        assert_eq!(reason, "two; the useful part");
    }

    #[test]
    fn a_silent_failure_is_reported_by_its_status() {
        assert_eq!(reason_from("exit 42"), "ssh exited with status 42");
    }

    #[test]
    fn a_connection_that_failed_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let tried = dir.path().join("tried").display().to_string();
        // Fails the first time and works the second, so a snapshot arriving at all proves the
        // watch reconnected on its own.
        let script = format!(
            "if [ -e {tried} ]; then echo '{}'; exec sleep 30; else touch {tried}; exit 1; fi",
            line("second-time", "writing")
        );

        let updates = run(shell(script), |seen| !snapshots(seen).is_empty());

        assert!(updates.iter().any(is_lost), "{updates:?}");
        assert_eq!(
            snapshots(&updates)[0][0].session,
            SessionId::new("second-time").unwrap()
        );
    }

    #[test]
    fn stopping_reaches_a_watch_blocked_on_a_remote_that_says_nothing() {
        let stop = Stop::new();
        let stopper = stop.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            stopper.stop();
        });

        let (done, updates) = mpsc::channel();
        thread::spawn(move || {
            let mut seen = Vec::new();
            // Long enough that this test would time out if stopping did not reach the child.
            watch(shell("sleep 60".to_owned()), &stop, |update| {
                seen.push(update)
            });
            let _ = done.send(seen);
        });
        let updates = updates
            .recv_timeout(PATIENCE)
            .expect("the watch never ended");

        // A stop is the user's own doing, so there is nothing to report as lost.
        assert!(!updates.iter().any(is_lost), "{updates:?}");
    }

    #[test]
    fn stopping_cuts_short_the_wait_between_retries() {
        let stop = Stop::new();
        let stopper = stop.clone();
        let (done, ended) = mpsc::channel();
        thread::spawn(move || {
            watch(shell("exit 1".to_owned()), &stop, |_| {});
            let _ = done.send(());
        });

        // Once it has failed, it is waiting; the wait is a second and this is well inside it.
        thread::sleep(Duration::from_millis(100));
        stopper.stop();

        ended
            .recv_timeout(Duration::from_millis(500))
            .expect("the watch sat out the retry wait after being stopped");
    }
}
