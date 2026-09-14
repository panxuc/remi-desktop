use remi_core::{record, source, store};

use crate::setup;

/// Everything that can make a `remi-hook` command fail.
///
/// Errors are logged with their `Display` alone, which prints only the outermost message.
/// So a variant that wraps another error must put the cause into its own message, e.g.
/// `#[error("writing {path}: {source}")]`, or the cause never reaches the log.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{0}` is not implemented yet")]
    NotImplemented(&'static str),
    #[error("no session id: pass --session, or run from a harness that sends one on stdin")]
    NoSession,
    #[error("cannot find this program's own path")]
    NoExePath,
    #[error("check found {0} problem(s)")]
    CheckFailed(usize),
    #[error(transparent)]
    Store(#[from] store::Error),
    #[error(transparent)]
    Record(#[from] record::Error),
    #[error(transparent)]
    Watch(#[from] source::local::Error),
    #[error(transparent)]
    Setup(#[from] setup::Error),
    #[error("writing to stdout: {0}")]
    Stdout(#[source] std::io::Error),
}
