//! Shared state-directory and clock helpers.
//!
//! Both the recency history ([`crate::history`]) and the update cache
//! ([`crate::update`]) keep a small file under the same XDG state directory and
//! stamp it with the same epoch clock. The two helpers lived byte-for-byte in
//! both modules; they are single-sourced here so the layout and the clock can
//! never drift between them.

use std::env;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// The plugin's state directory: `$XDG_STATE_HOME/herdr-switchboard`, falling back to
/// `~/.local/state/herdr-switchboard`. `None` when neither var is set — callers then
/// skip the file entirely, which is the "no history / no cache" degrade.
pub fn state_dir() -> Option<PathBuf> {
    let base = env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/state"))
        })?;
    Some(base.join("herdr-switchboard"))
}

/// A file inside [`state_dir`], or `None` when there is no state dir.
pub fn state_file(name: &str) -> Option<PathBuf> {
    Some(state_dir()?.join(name))
}

/// Seconds since the Unix epoch, or 0 if the clock is before it.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
