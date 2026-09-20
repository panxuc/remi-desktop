//! What every harness's hook input means to Remi once that harness has parsed its own JSON.
//!
//! The rules here are ours, not any harness's: they hold for a payload that has not been written
//! yet as much as for the two that have, which is why they live in one place rather than in each
//! harness's module.

use std::path::Path;

/// A record carries the last component of the session's working directory and nothing more — it
/// is a label for the menu, and the rest of the path is the user's business.
pub(super) fn cwd_name(cwd: &str) -> Option<String> {
    Some(Path::new(cwd).file_name()?.to_string_lossy().into_owned())
}

/// Stdin that is empty or all whitespace is not a malformed payload: the hook can be run by hand
/// with no input at all, and a harness that sends nothing is saying nothing, not saying junk.
pub(super) fn is_blank(json: &[u8]) -> bool {
    json.iter().all(u8::is_ascii_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_last_component_of_cwd() {
        assert_eq!(
            cwd_name("/home/u/repos/remi-desktop/").as_deref(),
            Some("remi-desktop")
        );
        assert_eq!(cwd_name("remi-desktop").as_deref(), Some("remi-desktop"));
        assert_eq!(cwd_name("/"), None);
    }

    #[test]
    fn blank_is_whitespace_or_nothing() {
        assert!(is_blank(b""));
        assert!(is_blank(b" \n"));
        assert!(!is_blank(br#"{"session_id":"s1"}"#));
    }
}
