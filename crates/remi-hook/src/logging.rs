use std::io::{self, IsTerminal};

use tracing_subscriber::EnvFilter;

/// Used when `RUST_LOG` is unset: a hook runs on every agent tool call, so it stays quiet
/// unless something is wrong.
const DEFAULT_FILTER: &str = "warn";

/// Installs the global subscriber. Verbosity comes from `RUST_LOG`, in `EnvFilter` syntax:
/// `RUST_LOG=debug`, or per crate, `RUST_LOG=warn,remi_core=trace`.
///
/// Logs go to stderr, never stdout: `watch` and `snapshot` write JSON to stdout, and a log
/// line mixed into it would corrupt the stream the pet parses.
pub fn init() {
    let (filter, invalid) = match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, None),
        Err(err) if std::env::var_os("RUST_LOG").is_some() => {
            (EnvFilter::new(DEFAULT_FILTER), Some(err))
        }
        Err(_) => (EnvFilter::new(DEFAULT_FILTER), None),
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_file(true)
        .with_line_number(true)
        .init();

    if let Some(err) = invalid {
        tracing::warn!(error = %err, "ignoring invalid RUST_LOG, using {DEFAULT_FILTER:?}");
    }
}
