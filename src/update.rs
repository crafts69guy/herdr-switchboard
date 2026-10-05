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
use std::path::{Path, PathBuf};
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
    read_cache_at(&cache_path()?)
}

fn read_cache_at(path: &Path) -> Option<(u64, String)> {
    let text = fs::read_to_string(path).ok()?;
    let (at, latest) = text.trim().split_once('\t')?;
    Some((at.parse().ok()?, latest.to_string()))
}

fn write_cache_at(path: &Path, checked_at: u64, latest: &str) -> Result<()> {
    // Through `state` rather than a hand-rolled temp-and-rename: the detached
    // refresh child and any picker that starts one both write this path, and
    // they were sharing a single fixed `update.tmp`.
    crate::state::write_private(path, format!("{checked_at}\t{latest}\n").as_bytes())
}

/// The newest version tagged on the remote.
fn fetch_latest() -> Option<String> {
    latest_from(
        Command::new("git")
            .args(["ls-remote", "--tags", "--refs", REPO])
            // Never let git stop to ask for credentials: this runs with no terminal.
            .env("GIT_TERMINAL_PROMPT", "0"),
    )
}

/// Run a tag listing and take its highest version; any failure is `None`.
fn latest_from(command: &mut Command) -> Option<String> {
    let out = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    latest_tag(&String::from_utf8_lossy(&out.stdout))
}

/// The highest version among `git ls-remote --tags --refs` lines
/// (`<sha>\trefs/tags/v0.5.0`), ignoring tags that are not versions.
fn latest_tag(ls_remote: &str) -> Option<String> {
    let latest = ls_remote
        .lines()
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
    newer_than(cfg, read_cache(), VERSION)
}

fn newer_than(cfg: &Config, cache: Option<(u64, String)>, current: &str) -> Option<String> {
    if !cfg.common.update_check {
        return None;
    }
    let (_, latest) = cache?;
    let latest_v = parse_version(&latest)?;
    (latest_v > parse_version(current)?).then_some(latest)
}

/// Whether a refresh should start: checks enabled and no reading younger than a day.
fn refresh_due(cfg: &Config, cache: Option<&(u64, String)>, now: u64) -> bool {
    cfg.common.update_check && cache.is_none_or(|(at, _)| now.saturating_sub(*at) >= TTL_SECS)
}

/// Kick off a refresh, if it is due, in a process that outlives this one.
pub fn spawn_refresh_if_stale(cfg: &Config) {
    refresh_if_due(cfg, read_cache(), now(), spawn_check);
}

/// Start `spawn` when a refresh is due; reports whether it was started.
fn refresh_if_due(
    cfg: &Config,
    cache: Option<(u64, String)>,
    now: u64,
    spawn: fn() -> std::io::Result<()>,
) -> bool {
    refresh_due(cfg, cache.as_ref(), now) && spawn().is_ok()
}

/// The detached `--update-check` child: its own process group, so closing the
/// pane does not signal it, and no stdio, so it can never draw on a terminal
/// the TUI owns.
fn spawn_check() -> std::io::Result<()> {
    Command::new(std::env::current_exe()?)
        .arg("--update-check")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
}

/// Entry point for `herdr-switchboard --update-check`: fetch, cache, exit. No UI.
pub fn main() -> Result<()> {
    let path = cache_path().ok_or_else(|| anyhow::anyhow!("no state dir"))?;
    record_check(&path, fetch_latest(), now())
}

/// Write what a check learned. A failed fetch still stamps the attempt, or every
/// launch re-spawns a child that cannot reach the network — an offline machine
/// would fork one per picker open — keeping whatever version was known before.
fn record_check(path: &Path, fetched: Option<String>, checked_at: u64) -> Result<()> {
    let latest = fetched.unwrap_or_else(|| {
        read_cache_at(path)
            .map(|(_, l)| l)
            .unwrap_or_else(|| VERSION.into())
    });
    write_cache_at(path, checked_at, &latest)
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
        assert_eq!(latest_tag(out).as_deref(), Some("0.10.0"));
        assert_eq!(latest_tag("000000\trefs/tags/nightly\n"), None);
    }

    fn config(update_check: bool) -> Config {
        let mut cfg = Config::default();
        cfg.common.update_check = update_check;
        cfg
    }

    #[test]
    fn only_a_strictly_newer_cached_version_is_announced() {
        let cache = |v: &str| Some((0, v.to_string()));
        assert_eq!(
            newer_than(&config(true), cache("1.2.0"), "1.1.9").as_deref(),
            Some("1.2.0")
        );
        assert_eq!(newer_than(&config(true), cache("1.1.9"), "1.1.9"), None);
        assert_eq!(newer_than(&config(true), cache("garbled"), "1.1.9"), None);
        assert_eq!(newer_than(&config(true), None, "1.1.9"), None);
        assert_eq!(newer_than(&config(false), cache("9.0.0"), "1.1.9"), None);
        // The running build's own version is always parseable.
        assert!(parse_version(VERSION).is_some());
    }

    #[test]
    fn a_refresh_is_due_once_a_day_and_never_when_checks_are_off() {
        let reading = (1_000, "1.0.0".to_string());
        assert!(refresh_due(&config(true), None, 5_000));
        assert!(!refresh_due(
            &config(true),
            Some(&reading),
            1_000 + TTL_SECS - 1
        ));
        assert!(refresh_due(&config(true), Some(&reading), 1_000 + TTL_SECS));
        assert!(!refresh_due(&config(false), None, 5_000));
    }

    #[test]
    fn a_check_records_what_it_found_and_stamps_a_failed_one() {
        let dir = std::env::temp_dir().join(format!("swb-update-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("update.tsv");

        // Offline with no prior reading: stamp the running version.
        record_check(&path, None, 10).unwrap();
        assert_eq!(read_cache_at(&path), Some((10, VERSION.to_string())));

        record_check(&path, Some("9.9.9".into()), 20).unwrap();
        assert_eq!(read_cache_at(&path), Some((20, "9.9.9".to_string())));

        // Offline again: a fresh stamp, but the known version survives.
        record_check(&path, None, 30).unwrap();
        assert_eq!(read_cache_at(&path), Some((30, "9.9.9".to_string())));

        fs::write(&path, "not a reading").unwrap();
        assert_eq!(read_cache_at(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Checks switched off reaches neither the cache nor a child process.
    #[test]
    fn nothing_runs_when_update_checks_are_off() {
        assert_eq!(available(&config(false)), None);
        spawn_refresh_if_stale(&config(false));
    }

    /// A due refresh starts the child; a fresh reading, checks switched off, or
    /// a child that would not start all report nothing started.
    #[test]
    fn a_due_refresh_starts_exactly_one_child() {
        assert!(refresh_if_due(&config(true), None, 5_000, || Ok(())));
        assert!(!refresh_if_due(
            &config(true),
            Some((5_000, "1.0.0".into())),
            5_001,
            || panic!("a fresh reading must not spawn")
        ));
        assert!(!refresh_if_due(&config(false), None, 5_000, || panic!(
            "checks are off"
        )));
        assert!(!refresh_if_due(&config(true), None, 5_000, || Err(
            std::io::Error::other("fork failed")
        )));
    }

    /// The listing is read from whatever command produced it: a tag list is
    /// parsed, and a failed or missing command is simply no answer.
    #[test]
    fn a_tag_listing_command_yields_its_highest_version() {
        let mut listing = Command::new("sh");
        listing.args([
            "-c",
            "printf 'a\\trefs/tags/v1.2.3\\nb\\trefs/tags/v1.10.0\\n'",
        ]);
        assert_eq!(latest_from(&mut listing).as_deref(), Some("1.10.0"));
        assert_eq!(latest_from(&mut Command::new("false")), None);
        assert_eq!(
            latest_from(&mut Command::new("switchboard-no-such-git")),
            None
        );
    }

    /// The detached child really starts. Here the executable is this test
    /// binary, which rejects `--update-check` and exits at once.
    #[test]
    fn the_update_check_child_starts_detached() {
        spawn_check().expect("the child starts");
    }
}
