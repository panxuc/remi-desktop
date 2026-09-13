/// Everything that can make a `remi-hook` command fail.
///
/// Errors are logged with their `Display` alone, which prints only the outermost message.
/// So a variant that wraps another error must put the cause into its own message, e.g.
/// `#[error("writing {path}: {source}")]`, or the cause never reaches the log.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{0}` is not implemented yet")]
    NotImplemented(&'static str),
}
