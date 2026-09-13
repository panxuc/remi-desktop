mod error;
mod logging;

use std::io::{self, IsTerminal, Write};
use std::panic;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use remi_core::harness::{self, HookInput};
use remi_core::record::{SessionId, SessionRecord};
use remi_core::signal::{SessionContext, Signal};
use remi_core::state::PetState;
use remi_core::store::{self, Store, Update};

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

impl Command {
    /// Commands a harness may call. These always exit 0: a non-zero exit from a `PreToolUse`
    /// hook can block the tool call, and a pet must never be able to stop the agent working.
    fn is_harness_path(&self) -> bool {
        matches!(self, Command::Signal(_) | Command::State(_))
    }
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

impl From<Harness> for harness::Harness {
    fn from(arg: Harness) -> Self {
        match arg {
            Harness::ClaudeCode => harness::Harness::ClaudeCode,
            Harness::OpenCode => harness::Harness::OpenCode,
        }
    }
}

/// A harness event, named so it means the same thing for every harness. Each adapter
/// translates its own hooks into these; only the reducer decides which pose they produce.
#[derive(Clone, Copy, ValueEnum)]
enum SignalEvent {
    /// The user submitted a prompt and the agent started on it. Remi: thinking.
    TurnStart,
    /// The agent called a tool that looks at code without changing it (read, grep, glob).
    /// Remi: viewing.
    ReadStart,
    /// The agent called a tool that changes files (edit, write). Remi: writing.
    EditStart,
    /// A tool call of either kind finished, whether or not it succeeded. Remi: thinking.
    /// On harnesses without an "approval answered" event, this is what clears the waiting
    /// pose.
    ToolEnd,
    /// The agent is blocked on the user approving something or answering a question.
    /// Remi: waiting for input. A second prompt before it clears keeps the original pose to
    /// return to.
    ApprovalAsked,
    /// The user answered an approval prompt, before the tool it guarded runs. Remi returns
    /// to the pose held before the prompt. For harnesses that report the answer directly.
    ApprovalAnswered,
    /// The agent finished its turn. Remi: proud, fading to idle.
    TurnEnd,
    /// The session closed. Its record is removed, and the pet shows it offline.
    SessionEnd,
}

impl From<SignalEvent> for Signal {
    fn from(event: SignalEvent) -> Self {
        match event {
            SignalEvent::TurnStart => Signal::TurnStart,
            SignalEvent::ReadStart => Signal::ReadStart,
            SignalEvent::EditStart => Signal::EditStart,
            SignalEvent::ToolEnd => Signal::ToolEnd,
            SignalEvent::ApprovalAsked => Signal::ApprovalAsked,
            SignalEvent::ApprovalAnswered => Signal::ApprovalAnswered,
            SignalEvent::TurnEnd => Signal::TurnEnd,
            SignalEvent::SessionEnd => Signal::SessionEnd,
        }
    }
}

#[derive(Args)]
struct StateArgs {
    pose: Pose,
    /// Session to write. The default keeps a test pose from overwriting a real agent session.
    #[arg(long, default_value = "manual")]
    session: String,
    #[arg(long, value_enum, default_value_t = Harness::ClaudeCode)]
    harness: Harness,
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

impl From<Pose> for PetState {
    fn from(pose: Pose) -> Self {
        match pose {
            Pose::Thinking => PetState::Thinking,
            Pose::Viewing => PetState::Viewing,
            Pose::Writing => PetState::Writing,
            Pose::WaitingForInput => PetState::WaitingForInput,
            Pose::Proud => PetState::Proud,
            Pose::Offline => PetState::Offline,
        }
    }
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

/// What commands read from the outside world, gathered once in `main` so that handlers take
/// it as input instead of reaching for the environment, the clock, or the process themselves.
struct Env {
    store: Store,
    /// Unix seconds when the command started.
    now: i64,
    /// Last component of the directory the hook was started in. Never the full path, which
    /// would reveal where the user keeps things.
    cwd_name: Option<String>,
}

impl Env {
    fn capture() -> Result<Self, Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let cwd_name = std::env::current_dir()
            .ok()
            .and_then(|dir| Some(dir.file_name()?.to_string_lossy().into_owned()));
        Ok(Self {
            store: Store::locate()?,
            now,
            cwd_name,
        })
    }
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

    let harness_path = cli.command.is_harness_path();
    let outcome =
        panic::catch_unwind(move || Env::capture().and_then(|env| run(cli.command, &env)));
    let succeeded = match outcome {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            tracing::error!("command failed: {err}");
            false
        }
        Err(_) => false, // the panic hook has already printed the message
    };
    if succeeded || harness_path {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The same commands as [`Command::is_harness_path`], matched on raw arguments because a
/// failed parse never produces a `Command`.
fn invoked_by_harness() -> bool {
    matches!(std::env::args().nth(1).as_deref(), Some("signal" | "state"))
}

fn run(command: Command, env: &Env) -> Result<(), Error> {
    match command {
        Command::Signal(args) => signal(args, env),
        Command::State(args) => state(args, env),
        Command::Watch => watch(),
        Command::Snapshot => snapshot(env),
        Command::Check => check(),
        Command::Setup(args) => setup(args),
        Command::Uninstall { purge } => uninstall(purge),
    }
}

fn signal(args: SignalArgs, env: &Env) -> Result<(), Error> {
    let harness = harness::Harness::from(args.harness);

    let stdin = io::stdin();
    // On a terminal, someone is typing the command by hand, and waiting for JSON they will
    // never send would look like a hang.
    let input = if stdin.is_terminal() {
        HookInput::default()
    } else {
        // Bad input still leaves the flags and the hook's own directory to go on.
        harness.read_input(stdin.lock()).unwrap_or_else(|err| {
            tracing::warn!("ignoring hook input: {err}");
            HookInput::default()
        })
    };

    let session = match args.session {
        Some(id) => SessionId::new(id)?,
        None => input.session.ok_or(Error::NoSession)?,
    };
    let previous = read_previous(&env.store, &session);
    let signal = Signal::from(args.event);
    let context = SessionContext {
        session,
        harness: harness.id().to_owned(),
        ts: env.now,
        cwd: input.cwd.or_else(|| env.cwd_name.clone()),
        title: args.title,
    };

    let update = signal.next_update(previous.as_ref(), context);
    tracing::debug!("{signal:?}: {update:?}");
    env.store.apply(update)?;
    prune(&env.store);
    Ok(())
}

fn state(args: StateArgs, env: &Env) -> Result<(), Error> {
    let session = SessionId::new(args.session)?;
    let previous = read_previous(&env.store, &session);

    let mut record = SessionRecord::following(
        previous.as_ref(),
        session,
        harness::Harness::from(args.harness).id(),
        args.pose.into(),
        env.now,
    );
    record.cwd = env.cwd_name.clone();

    env.store.apply(Update::Write(record))?;
    prune(&env.store);
    Ok(())
}

fn watch() -> Result<(), Error> {
    Err(Error::NotImplemented("watch"))
}

fn snapshot(env: &Env) -> Result<(), Error> {
    let mut records = env.store.list()?;
    records.sort_by(|a, b| a.session.cmp(&b.session));

    let json = serde_json::to_string(&records).expect("session records always serialize");
    writeln!(io::stdout().lock(), "{json}").map_err(Error::Stdout)
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

/// The session's current record, if it has a readable one. An unreadable file is treated as
/// no record: the write that follows replaces it, which is better than never writing again.
fn read_previous(store: &Store, session: &SessionId) -> Option<SessionRecord> {
    store.read(session).unwrap_or_else(|err| {
        tracing::warn!("ignoring unreadable previous record: {err}");
        None
    })
}

/// Every write also sweeps out sessions that died without ending. A failure here is only
/// logged: it must never cost the write that just succeeded.
fn prune(store: &Store) {
    match store.prune(store::PRUNE_AFTER) {
        Ok(0) => {}
        Ok(removed) => tracing::debug!("pruned {removed} stale session files"),
        Err(err) => tracing::warn!("pruning stale sessions failed: {err}"),
    }
}
