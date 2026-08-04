//! "A newer version exists" — told, never acted on.
//!
//! **The TUI never touches the network.** It reads a local cache file; a detached
//! `--update-check` child does the fetch and writes that cache. The picker often lives
//! under a second — prefix+space, three keystrokes, enter — so a thread inside it would
//! be killed mid-fetch and the cache would never land. A separate process outlives us.
//! The badge therefore appears on the *next* launch after a refresh, which for a daily
//! check is not a difference anyone can perceive.
//!
//! `git ls-remote` rather than the GitHub API: no `jq` (optional here by design), no
//! 60-requests-per-hour unauthenticated rate limit shared with every other tool on the
//! machine, no JSON, no auth. `bin/release.sh` creates the tag and the release together,
//! so a tag is an honest proxy for a release.
//!
//! Everything here fails silently. No network, no git, a rate limit, a garbled tag — the
//! switcher opens exactly as it always did.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::data::Config;
use crate::state::{now, state_file};

const REPO: &str = "https://github.com/crafts69guy/herdr-switchboard";
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// One check a day. The plugin does not move fast enough to justify more, and this is
/// the only outbound request it makes.
const TTL_SECS: u64 = 24 * 60 * 60;

/// A semver triple. Compared as numbers, never as text: `"0.10.0" < "0.9.0"` is true for
/// strings and false for versions, and this plugin will reach 0.10.0.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
struct Version(u64, u64, u64);

fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim().trim_start_matches('v');
    let mut parts = s.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let v = Version(next()?, next()?, next()?);
    // Reject trailing junk: `0.5.0.1` is not a version we understand.
    parts.next().is_none().then_some(v)
}

/// Beside the recency state, and for the same reason: it is cache, not configuration.
fn cache_path() -> Option<PathBuf> {
    state_file("update.tsv")
}

/// `checked_at<TAB>latest`, one line — the same shape and atomicity as `history.rs`.
fn read_cache() -> Option<(u64, String)> {
    let text = fs::read_to_string(cache_path()?).ok()?;
    let (at, latest) = text.trim().split_once('\t')?;
    Some((at.parse().ok()?, latest.to_string()))
}

fn write_cache(latest: &str) -> Result<()> {
    let p = cache_path().ok_or_else(|| anyhow::anyhow!("no state dir"))?;
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, format!("{}\t{}\n", now(), latest))?;
    fs::rename(&tmp, &p)?;
    Ok(())
}

/// The newest version tagged on the remote.
fn fetch_latest() -> Option<String> {
    let out = Command::new("git")
        .args(["ls-remote", "--tags", "--refs", REPO])
        // Never let git stop to ask for credentials: this runs with no terminal.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let latest = text
        .lines()
        // `<sha>\trefs/tags/v0.5.0`
        .filter_map(|l| l.split("refs/tags/").nth(1))
        .filter_map(parse_version)
        .max()?;
    Some(format!("{}.{}.{}", latest.0, latest.1, latest.2))
}

/// A newer version than the one running, if the cache knows of one.
///
/// The running version comes from `CARGO_PKG_VERSION`, never `herdr plugin list`: herdr
/// caches a plugin's manifest at link/install time and `reload-config` does not re-read
/// it, so that registry reported 0.3.3 for a 0.5.0 checkout.
pub fn available(cfg: &Config) -> Option<String> {
    if !cfg.bool("update_check", true) {
        return None;
    }
    let (_, latest) = read_cache()?;
    let latest_v = parse_version(&latest)?;
    let current = parse_version(VERSION)?;
    (latest_v > current).then_some(latest)
}

/// Kick off a refresh, if it is due, in a process that outlives this one.
pub fn spawn_refresh_if_stale(cfg: &Config) {
    if !cfg.bool("update_check", true) {
        return;
    }
    if let Some((at, _)) = read_cache() {
        if now().saturating_sub(at) < TTL_SECS {
            return;
        }
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // Detached: its own process group, so closing the pane does not signal it, and no
    // stdio, so it can never draw on a terminal the TUI owns.
    let _ = Command::new(exe)
        .arg("--update-check")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

/// Entry point for `herdr-switchboard --update-check`: fetch, cache, exit. No UI.
pub fn main() -> Result<()> {
    if let Some(latest) = fetch_latest() {
        write_cache(&latest)?;
    } else {
        // Stamp the attempt anyway, or every launch re-spawns a child that cannot reach
        // the network — an offline machine would fork one per picker open.
        let keep = read_cache()
            .map(|(_, l)| l)
            .unwrap_or_else(|| VERSION.into());
        write_cache(&keep)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically_not_as_text() {
        // The whole reason this is a triple: "0.10.0" sorts before "0.9.0" as a string.
        assert!(parse_version("0.10.0") > parse_version("0.9.0"));
        assert!(parse_version("v1.0.0") > parse_version("0.99.99"));
        assert_eq!(parse_version("v0.5.0"), parse_version("0.5.0"));
    }

    #[test]
    fn parse_rejects_things_that_are_not_versions() {
        assert!(parse_version("").is_none());
        assert!(parse_version("0.5").is_none());
        assert!(parse_version("0.5.0.1").is_none());
        assert!(parse_version("latest").is_none());
        assert!(parse_version("v0.5.x").is_none());
    }

    #[test]
    fn ls_remote_output_yields_the_highest_tag() {
        // The real shape of `git ls-remote --tags --refs`, deliberately out of order and
        // carrying a tag that is not a version.
        let out = "\
abc123\trefs/tags/v0.4.0
def456\trefs/tags/v0.10.0
789abc\trefs/tags/v0.9.0
000000\trefs/tags/nightly
";
        let latest = out
            .lines()
            .filter_map(|l| l.split("refs/tags/").nth(1))
            .filter_map(parse_version)
            .max()
            .unwrap();
        assert_eq!(latest, Version(0, 10, 0));
    }
}
