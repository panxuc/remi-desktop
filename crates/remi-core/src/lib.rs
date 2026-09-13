//! The whole domain of Remi: states, the session record, the reducer, the registry, and the
//! transports. No UI dependencies may enter this crate — keeping it UI-free is what lets the
//! GUI toolkit be swapped without touching any of the logic.
//!
//! Error messages include their cause (`"reading {path}: {source}"`), because callers log an
//! error with its `Display` alone.

pub mod harness;
pub mod record;
pub mod signal;
pub mod state;
pub mod store;
