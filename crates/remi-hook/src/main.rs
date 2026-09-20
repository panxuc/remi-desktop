mod error;
mod logging;
mod setup;

use std::io::{self, IsTerminal, Write};
use std::panic;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use remi_core::harness::{self, HookInput};
use remi_core::record::{HarnessId, SessionId, SessionRecord};
use remi_core::signal::{SessionContext, Signal};
use remi_core::source::local::StateDirWatch;
use remi_core::state::PetState;
use remi_core::store::{self, Store, Update};

use crate::error::Error;
use crate::setup::Saved;

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
    /// Print every live session as one JSON array, then again each time any of them changes.
    /// Run by the pet over ssh; stops when stdin closes.
    Watch,
    /// Print live records as one JSON array, then exit.
    Snapshot,
    /// Verify this machine is set up: print this program's path, the state dir, and whether
    /// the selected harness's hooks are in place, and exit non-zero if something is wrong.
    Check {
        /// Harness whose installed hooks should be checked.
        #[arg(long, value_enum, default_value_t = Harness::ClaudeCode)]
        harness: Harness,
    },
    /// Add remi's hooks to this machine's harness configuration, running this copy of
    /// `remi-hook`. Safe to run again; the previous file is kept as a `.bak`.
    Setup(SetupArgs),
    /// Remove exactly what `setup` added.
    Uninstall {
        /// Harness whose Remi hooks should be removed.
        #[arg(long, value_enum, default_value_t = Harness::ClaudeCode)]
        harness: Harness,
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

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Harness {
    /// Claude Code, through hooks in its settings.json.
    ClaudeCode,
    /// Codex, through lifecycle hooks in hooks.json.
    Codex,
    /// OpenCode, through a plugin.
    #[value(name = "opencode")]
    OpenCode,
}

impl From<Harness> for harness::Harness {
    fn from(arg: Harness) -> Self {
        match arg {
            Harness::ClaudeCode => harness::Harness::ClaudeCode,
            Harness::Codex => harness::Harness::Codex,
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
    /// Reply text from the agent is appearing for the user; sent again for each piece as it
    /// streams. Remi: replying.
    Reply,
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
    /// The user interrupted the turn. Remi rests until work resumes.
    TurnInterrupted,
    /// The session closed. Its record is removed, and the pet shows it offline.
    SessionEnd,
}

impl From<SignalEvent> for Signal {
    fn from(event: SignalEvent) -> Self {
        match event {
            SignalEvent::TurnStart => Signal::TurnStart,
            SignalEvent::ReadStart => Signal::ReadStart,
            SignalEvent::EditStart => Signal::EditStart,
            SignalEvent::Reply => Signal::Reply,
            SignalEvent::ToolEnd => Signal::ToolEnd,
            SignalEvent::ApprovalAsked => Signal::ApprovalAsked,
            SignalEvent::ApprovalAnswered => Signal::ApprovalAnswered,
            SignalEvent::TurnEnd => Signal::TurnEnd,
            SignalEvent::TurnInterrupted => Signal::TurnInterrupted,
            SignalEvent::SessionEnd => Signal::SessionEnd,
        }
    }
}

#[derive(Args)]
struct StateArgs {
    pose: Pose,
    /// Session to write. The default keeps a test pose from overwriting a real agent session.
    /// Nothing ends it: clear it with `signal session-end --harness <harness> --session <id>`,
    /// or it is pruned after a day without writes.
    #[arg(long, default_value = "manual")]
    session: String,
    #[arg(long, value_enum, default_value_t = Harness::ClaudeCode)]
    harness: Harness,
}

/// A pose that can be set manually. Interrupted turns also write Idle through the reducer.
#[derive(Clone, Copy, ValueEnum)]
enum Pose {
    /// Working out what to do next.
    Thinking,
    /// Reading code.
    Viewing,
    /// Changing files.
    Writing,
    /// Writing its reply to the user.
    Replying,
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
            Pose::Replying => PetState::Replying,
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

/// The outside world as it was when the command started: handlers take this instead of
/// reaching for the environment, the clock, or the process themselves.
struct Env {
    store: Store,
    /// Unix seconds when the command started.
    now: i64,
    /// Last component of the directory the hook was started in, never the full path.
    cwd_name: Option<String>,
    /// This program's own path, which is what `setup` makes hooks run. `None` if the OS won't
    /// say.
    exe: Option<PathBuf>,
    /// Claude Code's user settings file.
    claude_settings: PathBuf,
    codex_settings: PathBuf,
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
            exe: std::env::current_exe().ok(),
            claude_settings: setup::claude::settings_path()?,
            codex_settings: setup::codex::settings_path()?,
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

    let codex_hook =
        matches!(&cli.command, Command::Signal(args) if args.harness == Harness::Codex);
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
    if codex_hook {
        let _ = print_line("{}");
    }
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
        Command::Watch => watch(env),
        Command::Snapshot => snapshot(env),
        Command::Check { harness } => check(harness, env),
        Command::Setup(args) => setup(args, env),
        Command::Uninstall { harness, purge } => uninstall(harness, purge, env),
    }
}

fn signal(args: SignalArgs, env: &Env) -> Result<(), Error> {
    let harness = harness::Harness::from(args.harness);
    let harness_id = harness.id();

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
    let previous = read_previous(&env.store, &harness_id, &session);
    let signal = Signal::from(args.event);
    let context = SessionContext {
        session,
        harness: harness_id,
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
    let harness = harness::Harness::from(args.harness).id();
    let session = SessionId::new(args.session)?;
    let previous = read_previous(&env.store, &harness, &session);

    let mut record = SessionRecord::following(
        previous.as_ref(),
        session,
        harness,
        args.pose.into(),
        env.now,
    );
    record.cwd = env.cwd_name.clone();

    env.store.apply(Update::Write(record))?;
    prune(&env.store);
    Ok(())
}

fn watch(env: &Env) -> Result<(), Error> {
    let (mut dir_watch, mut listing) = StateDirWatch::start(env.store.clone())?;
    let mut watching_stdin = false;

    loop {
        match print_records(&listing) {
            Ok(()) => {}
            // Nobody is reading any more, which is often how a dropped connection shows first.
            Err(Error::Stdout(err)) if err.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            Err(err) => return Err(err),
        }

        // Whoever runs `watch` holds its stdin open for as long as it wants output — for the
        // pet, that is the ssh connection — so stdin closing is the signal to stop. Started
        // only after the first listing is out, so a stdin that is already closed still gets
        // that listing before the exit.
        if !watching_stdin {
            thread::spawn(|| {
                let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
                std::process::exit(0);
            });
            watching_stdin = true;
        }

        listing = dir_watch.next_snapshot()?;
    }
}

fn snapshot(env: &Env) -> Result<(), Error> {
    print_records(&env.store.list()?)
}

/// Prints what a pet will find on this machine, and fails if anything would stop it working.
/// Keeps going after a problem, so one run shows all of them.
fn check(harness: Harness, env: &Env) -> Result<(), Error> {
    let mut problems = 0;

    match &env.exe {
        Some(exe) => print_line(&format!(
            "remi-hook {} at {}",
            env!("CARGO_PKG_VERSION"),
            exe.display()
        ))?,
        None => {
            problems += 1;
            print_line("remi-hook: cannot find this program's own path")?;
        }
    }

    match env.store.list() {
        Ok(records) => print_line(&format!(
            "state dir: {} ({} sessions)",
            env.store.dir().display(),
            records.len()
        ))?,
        Err(err) => {
            problems += 1;
            print_line(&format!("state dir: {err}"))?;
        }
    }

    let name = harness::Harness::from(harness).id();
    let settings = match harness {
        Harness::ClaudeCode => &env.claude_settings,
        Harness::Codex => &env.codex_settings,
        Harness::OpenCode => return Err(Error::NotImplemented("check --harness opencode")),
    };
    let inspect = match harness {
        Harness::Codex => setup::codex::inspect,
        _ => setup::claude::inspect,
    };
    let path = settings.display();
    match env.exe.as_deref() {
        None => print_line(&format!(
            "{name}: {path}: not checked, since hooks must name this program's path"
        ))?,
        Some(exe) => match inspect(settings, exe) {
            Err(err) => {
                problems += 1;
                print_line(&format!("{name}: {err}"))?;
            }
            Ok(inspection) if inspection.missing.is_empty() && inspection.unexpected.is_empty() => {
                print_line(&format!(
                    "{name}: all {} hooks set up in {path}",
                    inspection.expected
                ))?
            }
            Ok(inspection) => {
                problems += inspection.missing.len() + inspection.unexpected.len();
                print_line(&format!("{name}: {path}"))?;
                for hook in &inspection.missing {
                    print_line(&format!("  missing: {hook}"))?;
                }
                for hook in &inspection.unexpected {
                    print_line(&format!("  unexpected: {hook}"))?;
                }
                print_line(&format!("  run `remi-hook setup --harness {name}` to fix"))?;
            }
        },
    }

    if problems == 0 {
        Ok(())
    } else {
        Err(Error::CheckFailed(problems))
    }
}

fn setup(args: SetupArgs, env: &Env) -> Result<(), Error> {
    if args.forward.is_some() {
        return Err(Error::NotImplemented("setup --forward"));
    }
    let exe = env.exe.as_deref().ok_or(Error::NoExePath)?;

    let name = harness::Harness::from(args.harness).id();
    let edit = match args.harness {
        Harness::ClaudeCode => setup::claude::install(&env.claude_settings, exe)?,
        Harness::Codex => setup::codex::install(&env.codex_settings, exe)?,
        Harness::OpenCode => return Err(Error::NotImplemented("setup --harness opencode")),
    };
    match &edit.saved {
        Saved::Unchanged => print_line(&format!(
            "{name}: already set up in {}",
            edit.path.display()
        ))?,
        Saved::Written { backup } => {
            print_line(&format!(
                "{name}: wrote {} hooks to {}, running {}",
                edit.added,
                edit.path.display(),
                exe.display()
            ))?;
            if edit.removed > 0 {
                print_line(&format!("  replaced {} earlier remi hooks", edit.removed))?;
            }
            if let Some(backup) = backup {
                print_line(&format!(
                    "  previous settings saved as {}",
                    backup.display()
                ))?;
            }
        }
    }

    if args.harness == Harness::Codex {
        print_line(
            "codex: restart Codex and review/trust the Remi hooks with /hooks before they can run; check verifies the file, not Codex trust",
        )?;
    }
    if args.check {
        check(args.harness, env)?;
    }
    Ok(())
}

fn uninstall(harness: Harness, purge: bool, env: &Env) -> Result<(), Error> {
    if purge {
        return Err(Error::NotImplemented("uninstall --purge"));
    }

    let name = harness::Harness::from(harness).id();
    let edit = match harness {
        Harness::ClaudeCode => setup::claude::uninstall(&env.claude_settings)?,
        Harness::Codex => setup::codex::uninstall(&env.codex_settings)?,
        Harness::OpenCode => return Err(Error::NotImplemented("uninstall --harness opencode")),
    };
    match &edit.saved {
        Saved::Unchanged => {
            print_line(&format!("{name}: no remi hooks in {}", edit.path.display()))
        }
        Saved::Written { backup } => {
            print_line(&format!(
                "{name}: removed {} remi hooks from {}",
                edit.removed,
                edit.path.display()
            ))?;
            if let Some(backup) = backup {
                print_line(&format!(
                    "  previous settings saved as {}",
                    backup.display()
                ))?;
            }
            Ok(())
        }
    }
}

/// Writes `records` to stdout as one JSON array on a line of its own.
fn print_records(records: &[SessionRecord]) -> Result<(), Error> {
    print_line(&serde_json::to_string(records).expect("session records always serialize"))
}

/// Writes `line` to stdout, flushed straight away so a reader at the other end of a pipe sees
/// it immediately.
fn print_line(line: &str) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .map_err(Error::Stdout)
}

/// The session's current record, if it has a readable one. An unreadable file is treated as
/// no record: the write that follows replaces it, which is better than never writing again.
fn read_previous(store: &Store, harness: &HarnessId, session: &SessionId) -> Option<SessionRecord> {
    store.read(harness, session).unwrap_or_else(|err| {
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
