mod error;
mod logging;

use std::panic;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::error::Error;

#[derive(Parser)]
#[command(version, about = "Writes agent session state for the Remi desktop pet")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record something the agent harness just did. What harness hooks call.
    Signal(SignalArgs),
    /// Set a session's pose directly, bypassing the event rules. For testing the pet without
    /// running an agent.
    State(StateArgs),
    /// Stream live records as NDJSON: a snapshot, then deltas. Run by the pet over ssh.
    Watch,
    /// Print live records as one JSON array, then exit.
    Snapshot,
    /// Verify this machine is set up: print the resolved state dir, config, and harness
    /// capabilities, and exit non-zero if something is wrong.
    Check,
    /// Write this machine's harness configuration. Idempotent.
    Setup(SetupArgs),
    /// Remove exactly what `setup` wrote.
    Uninstall {
        /// Also remove the binary and the state dir.
        #[arg(long)]
        purge: bool,
    },
}

// The session's folder is not a flag: it comes from the `cwd` in the harness's stdin JSON,
// falling back to the directory the hook was started in.
#[derive(Args)]
struct SignalArgs {
    event: SignalEvent,
    /// How remi-hook should interpret the input from stdin.
    #[arg(long)]
    harness: Harness,
    /// Session id. Overrides the one in the harness's stdin; for callers that pass flags
    /// instead of JSON, and for testing by hand.
    #[arg(long)]
    session: Option<String>,
    /// Session title shown in the pet's menu. Overrides any title found from the harness.
    #[arg(long)]
    title: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Harness {
    /// Claude Code, through hooks in its settings.json.
    ClaudeCode,
    /// OpenCode, through a plugin.
    #[value(name = "opencode")]
    OpenCode,
}

/// A harness event, named so it means the same thing for every harness. Each adapter
/// translates its own hooks into these; only the reducer decides which pose they produce.
#[derive(Clone, Copy, ValueEnum)]
enum SignalEvent {
    /// A session opened. Registers it before its first turn.
    SessionStart,
    /// The user submitted a prompt and the agent started on it. Remi: thinking.
    TurnStart,
    /// The agent called a tool that looks at code without changing it (read, grep, glob).
    /// Remi: viewing.
    ReadStart,
    /// The agent called a tool that changes files (edit, write). Remi: writing.
    EditStart,
    /// A tool call of either kind finished. If an approval prompt was outstanding, Remi returns to the pose
    /// held before it; otherwise the agent is deciding what to do next. Remi: thinking.
    /// On harnesses without an "approval granted" event, this is what clears the waiting pose.
    ToolEnd,
    /// The agent is blocked on the user approving something or answering a question.
    /// Remi: waiting for input. A second prompt before it clears keeps the original pose to
    /// return to.
    ApprovalAsked,
    /// The user answered an approval prompt. Same effect as `tool-end`, for harnesses that
    /// report the answer directly.
    ApprovalAnswered,
    /// The agent finished its turn. Remi: proud, fading to idle.
    TurnEnd,
    /// The session closed. Remi: offline, and the session's record is removed.
    SessionEnd,
}

#[derive(Args)]
struct StateArgs {
    pose: Pose,
    /// Session to write. The default keeps a test pose from overwriting a real agent session.
    #[arg(long, default_value = "manual")]
    session: String,
}

/// A pose a session record can carry. Idle is not one of them: the pet shows idle on its
/// own once a session has gone quiet, so nothing ever writes it.
#[derive(Clone, Copy, ValueEnum)]
enum Pose {
    /// Working out what to do next.
    Thinking,
    /// Reading code.
    Viewing,
    /// Changing files.
    Writing,
    /// Blocked on the user.
    WaitingForInput,
    /// Just finished a turn; fades to idle.
    Proud,
    /// The session has ended.
    Offline,
}

#[derive(Args)]
struct SetupArgs {
    #[arg(long, value_enum, default_value_t = Harness::ClaudeCode)]
    harness: Harness,
    /// MQTT broker to forward records to, e.g. mqtts://host:8883.
    #[arg(long)]
    forward: Option<String>,
    /// Run `check` afterwards.
    #[arg(long)]
    check: bool,
}

fn main() -> ExitCode {
    logging::init();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            // A harness invoking us with bad arguments must still get 0; a human typo in
            // `setup` must not, or install.sh would report success.
            return if invoked_by_harness() || !err.use_stderr() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            };
        }
    };

    match cli.command {
        Command::Signal(args) => never_fail(|| signal(args)),
        Command::State(args) => never_fail(|| state(args)),
        Command::Watch => report(watch()),
        Command::Snapshot => report(snapshot()),
        Command::Check => report(check()),
        Command::Setup(args) => report(setup(args)),
        Command::Uninstall { purge } => report(uninstall(purge)),
    }
}

fn invoked_by_harness() -> bool {
    matches!(std::env::args().nth(1).as_deref(), Some("signal" | "state"))
}

/// Exit code is always 0 on the harness path: a non-zero exit from a `PreToolUse` hook can
/// block the tool call, and a pet must never be able to stop the agent working.
fn never_fail(f: impl FnOnce() -> Result<(), Error> + panic::UnwindSafe) -> ExitCode {
    match panic::catch_unwind(f) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::error!("command failed: {err}"),
        Err(_) => {} // the panic hook has already printed the message
    }
    ExitCode::SUCCESS
}

fn report(result: Result<(), Error>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("command failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn signal(_args: SignalArgs) -> Result<(), Error> {
    Err(Error::NotImplemented("signal"))
}

fn state(_args: StateArgs) -> Result<(), Error> {
    Err(Error::NotImplemented("state"))
}

fn watch() -> Result<(), Error> {
    Err(Error::NotImplemented("watch"))
}

fn snapshot() -> Result<(), Error> {
    Err(Error::NotImplemented("snapshot"))
}

fn check() -> Result<(), Error> {
    Err(Error::NotImplemented("check"))
}

fn setup(_args: SetupArgs) -> Result<(), Error> {
    Err(Error::NotImplemented("setup"))
}

fn uninstall(_purge: bool) -> Result<(), Error> {
    Err(Error::NotImplemented("uninstall"))
}
