//! External reads and response parsing for the Git menu.

use std::env;

use super::menu_config::parse_menu_conf;
use super::{Custom, ListKind, Row};
use crate::runner::CommandRunner;

/// The base branch a `review branch` diffs against: the configured `base_branch`
/// when it resolves, else the first of the conventional names that does. `None`
/// when the repo has none of them (a fresh repo with no `main`/`master`).
pub(super) fn detect_base_branch(
    runner: &dyn CommandRunner,
    cwd: &str,
    configured: &str,
) -> Option<String> {
    // `capture`, not `ok`: `ok` runs through `status`, which inherits the
    // terminal, and `rev-parse --verify` prints the resolved SHA on success —
    // that 40-char line was landing on the screen under the git overlay. `capture`
    // pipes stdout (and stderr), so the check stays silent; we only want the exit.
    let resolves = |r: &str| {
        runner
            .capture("git", &["-C", cwd, "rev-parse", "--verify", "--quiet", r])
            .is_some()
    };
    if !configured.is_empty() && resolves(configured) {
        return Some(configured.to_string());
    }
    ["main", "master", "origin/main", "origin/master"]
        .into_iter()
        .find(|r| resolves(r))
        .map(str::to_string)
}

/// How many files an all-files review would read: `git ls-files`, counted.
/// `None` when git could not answer — the review then opens without asking, the
/// same way an unreadable sub-list opens empty rather than erroring.
///
/// Counted rather than parsed, so a path containing a newline over-counts by
/// one; the only consequence is a confirmation the user would have seen anyway.
pub(super) fn count_tracked_files(runner: &dyn CommandRunner, cwd: &str) -> Option<usize> {
    runner
        .capture("git", &["-C", cwd, "ls-files"])
        .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
}

/// Fetch a sub-list. The two remote sources speak JSON and are parsed with
/// `serde_json` — **never `jq`**, which this repo does not depend on and must
/// not start depending on for a list that is only ever displayed. The conflict
/// list is `git status` porcelain, a stable line format that needs no parser.
///
/// Every failure (binary missing, not a GitHub remote, unparseable output) is an
/// empty list. The card then says so, which is the same thing the user needs to
/// know as an error would tell them, without a second failure surface.
pub(super) fn load_rows(runner: &dyn CommandRunner, cwd: &str, kind: ListKind) -> Vec<Row> {
    match kind {
        ListKind::PullRequests => load_pull_requests(runner, cwd),
        ListKind::Reviews => load_reviews(runner, cwd),
        ListKind::Conflicts => load_conflicts(runner, cwd),
    }
}

/// The unmerged files, from `git status --porcelain`.
///
/// One command gives both the path and the status code, where
/// `git diff --name-only --diff-filter=U` gives only the path — and "deleted by
/// them" versus "both modified" is exactly what decides whether opening the file
/// is the right move. `core.quotePath=false` keeps a non-ASCII path readable
/// instead of octal-escaped.
///
/// Porcelain paths are printed from the **repo root** even when git runs inside a
/// subdirectory, which is why `review.sh` resolves the pick against
/// `rev-parse --show-toplevel` rather than against the pane's cwd.
fn load_conflicts(runner: &dyn CommandRunner, cwd: &str) -> Vec<Row> {
    let Some(out) = runner.capture(
        "git",
        &[
            "-C",
            cwd,
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain",
        ],
    ) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            // `XY<space>path`: the code is the first two columns, so a path is only
            // reachable on a line with something after them.
            if line.len() < 4 || !line.is_char_boundary(2) {
                return None;
            }
            let (code, rest) = line.split_at(2);
            let detail = unmerged_detail(code)?;
            let path = rest.trim_start();
            if path.is_empty() {
                return None;
            }
            Some(Row {
                id: path.to_string(),
                // The body column clips an id at `ID_COL`; the pinned header draws
                // the label in full, so a deep path stays readable somewhere.
                label: path.to_string(),
                meta: String::new(),
                detail: detail.to_string(),
            })
        })
        .collect()
}

/// The seven porcelain codes git calls unmerged, spelled the way `git status`
/// spells them in its long form. Anything else — a staged edit, an untracked
/// file — is not a conflict and is not offered.
fn unmerged_detail(code: &str) -> Option<&'static str> {
    Some(match code {
        "DD" => "both deleted",
        "AU" => "added by us",
        "UD" => "deleted by them",
        "UA" => "added by them",
        "DU" => "deleted by us",
        "AA" => "both added",
        "UU" => "both modified",
        _ => return None,
    })
}

/// Run a command that returns a JSON array and map each element to a [`Row`].
/// Anything that is not an array of objects — a `gh` usage error, a missing
/// binary, an object where a list was expected — is no rows.
fn json_rows(out: Option<String>, row_of: impl Fn(&serde_json::Value) -> Option<Row>) -> Vec<Row> {
    let Some(json) = out
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    else {
        return Vec::new();
    };
    let Some(items) = json.as_array() else {
        return Vec::new();
    };
    items.iter().filter_map(row_of).collect()
}

/// `gh pr list` as rows: number, title, author, and the date it last moved.
///
/// Run through `sh -c` with a `cd` rather than `gh --repo <cwd>`: `--repo` takes
/// an `OWNER/REPO` coordinate or a URL, **not a path**, so passing the checkout
/// there is a usage error — and one that would show up as "no open pull
/// requests", indistinguishable from the truth. `gh` resolves the repo from its
/// working directory's remote on its own.
fn load_pull_requests(runner: &dyn CommandRunner, cwd: &str) -> Vec<Row> {
    let script = format!(
        "cd {} && gh pr list --limit 50 --json number,title,author,updatedAt",
        shell_quote(cwd)
    );
    json_rows(runner.capture("sh", &["-c", &script]), |pr| {
        let number = pr.get("number")?.as_u64()?;
        Some(Row {
            id: number.to_string(),
            label: pr
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            meta: day_of(pr.get("updatedAt").and_then(|v| v.as_str()).unwrap_or("")),
            detail: pr
                .get("author")
                .and_then(|a| a.get("login"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    })
}

/// Single-quote a string for `sh -c`. The only input is a repository path we were
/// handed, but a path with a quote in it must become an unreadable path, never a
/// second command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `tuicr review list` as rows: the session slug is the id (it is what
/// `review comments --session` takes), with the comment count as the detail.
/// Here `--repo` **does** take a checkout path — tuicr documents it that way.
fn load_reviews(runner: &dyn CommandRunner, cwd: &str) -> Vec<Row> {
    json_rows(
        runner.capture("tuicr", &["review", "list", "--repo", cwd]),
        |s| {
            let slug = s.get("slug")?.as_str()?.to_string();
            let count = s.get("comment_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let anchor = s
                .get("anchor")
                .and_then(|v| v.as_str())
                .filter(|a| !a.is_empty())
                .unwrap_or("(no anchor)");
            Some(Row {
                id: slug,
                label: anchor.to_string(),
                meta: day_of(s.get("updated_at").and_then(|v| v.as_str()).unwrap_or("")),
                detail: match count {
                    1 => "1 comment".to_string(),
                    n => format!("{n} comments"),
                },
            })
        },
    )
}

/// The date out of an ISO-8601 timestamp — the whole string is too wide for the
/// column and the clock half of it never decides anything.
fn day_of(timestamp: &str) -> String {
    timestamp
        .split('T')
        .next()
        .unwrap_or(timestamp)
        .chars()
        .take(10)
        .collect()
}

/// Whether `dir` sits inside a git working tree.
fn is_repo(runner: &dyn CommandRunner, dir: &str) -> bool {
    runner
        .capture("git", &["-C", dir, "rev-parse", "--git-dir"])
        .is_some()
}

/// The repo the menu acts on: the pane's own cwd, which herdr sets from the pane
/// `Prefix + g` was pressed in. `SWITCHBOARD_ORIGIN_CWD` is the fallback for the case
/// where the pane started somewhere else — it is a hint, not an authority, so it
/// is only taken when it is a repo and the cwd is not.
pub(super) fn repo_cwd(runner: &dyn CommandRunner) -> Option<String> {
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    if is_repo(runner, &cwd) {
        return Some(cwd);
    }
    env::var("SWITCHBOARD_ORIGIN_CWD")
        .ok()
        .filter(|s| !s.is_empty() && is_repo(runner, s))
}

/// Read the `menu.conf` custom rows from the plugin's config dir, if any.
pub(super) fn read_menu_conf() -> Vec<Custom> {
    let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").unwrap_or_default();
    if dir.is_empty() {
        return Vec::new();
    }
    std::fs::read_to_string(std::path::Path::new(&dir).join("menu.conf"))
        .map(|t| parse_menu_conf(&t))
        .unwrap_or_default()
}
