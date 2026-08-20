//! Accept dispatch — runs AFTER the TUI is torn down, so interactive bits
//! (clone prompt, remove confirm, update output) use the normal pane.

use std::env;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::{anyhow, Result};

use crate::data::{Config, Entry, Kind};
use crate::git::ReviewSpec;
use crate::runner::CommandRunner;

/// The targets `open_repo` understands.
fn is_open_target(t: &str) -> bool {
    matches!(t, "workspace" | "tab" | "split" | "pane")
}

/// `SWITCHBOARD_FORCE_TARGET`, set by `bin/action.sh` for the hot-path actions
/// (`open-workspace` / `open-tab` / `open-split`) so a dedicated key always
/// lands the repo in one place regardless of `default_target`.
pub fn forced_target() -> Option<String> {
    env::var("SWITCHBOARD_FORCE_TARGET")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Where Enter opens a repo: a forced target wins, then `default_target`.
/// Unrecognised values on either side fall back to `workspace` rather than
/// failing the open — the same leniency `bin/get.sh` applies.
pub fn resolve_default_target(forced: Option<&str>, configured: &str) -> String {
    forced
        .filter(|t| is_open_target(t))
        .or(Some(configured).filter(|t| is_open_target(t)))
        .unwrap_or("workspace")
        .to_string()
}

/// Which accept key was pressed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Accept {
    Default,
    Workspace,
    Tab,
    Split,
    Pane,
    Update,
    Remove,
    Clone,
    UpdatePlugin,
}

pub fn dispatch(
    runner: &dyn CommandRunner,
    entry: Option<Entry>,
    accept: Accept,
    origin_pane: &str,
    cfg: &Config,
    script_dir: &str,
    default_target: &str,
) -> Result<()> {
    if accept == Accept::Clone {
        // Hand the whole terminal to the bash clone flow.
        let err = Command::new("bash")
            .arg(format!("{script_dir}/get.sh"))
            .exec();
        return Err(anyhow!("failed to exec get.sh: {err}"));
    }

    // Must replace this process, not spawn beside it: `herdr plugin install` rewrites
    // the checkout holding the very binary running here. Needs no selection either.
    if accept == Accept::UpdatePlugin {
        let err = Command::new("bash")
            .arg(format!("{script_dir}/update-plugin.sh"))
            .exec();
        return Err(anyhow!("failed to exec update-plugin.sh: {err}"));
    }

    let e = entry.ok_or_else(|| anyhow!("no selection"))?;

    match e.kind {
        Kind::Workspace => focus_workspace(runner, &e.id),
        Kind::Agent => {
            let target = open_kind(accept);
            match (target, e.dir.as_deref()) {
                (Some(t), Some(dir)) => open_repo(runner, t, dir, origin_pane, &e.label, cfg),
                _ => focus_agent(runner, &e.id),
            }
        }
        Kind::Repo | Kind::Worktree => {
            let dir = e.dir.clone().unwrap_or_default();
            match accept {
                Accept::Default => {
                    open_repo(runner, default_target, &dir, origin_pane, &e.label, cfg)
                }
                Accept::Workspace => {
                    open_repo(runner, "workspace", &dir, origin_pane, &e.label, cfg)
                }
                Accept::Tab => open_repo(runner, "tab", &dir, origin_pane, &e.label, cfg),
                Accept::Split => open_repo(runner, "split", &dir, origin_pane, &e.label, cfg),
                Accept::Pane => open_repo(runner, "pane", &dir, origin_pane, &e.label, cfg),
                Accept::Update if e.kind == Kind::Repo => update(runner, &e.id, &e.label),
                Accept::Remove if e.kind == Kind::Repo => remove(runner, &dir, &e.label),
                Accept::Update | Accept::Remove => {
                    Err(anyhow!("update/remove is not supported for worktrees"))
                }
                // A clone / update-plugin `exec`s its own script before this
                // match, so neither reaches here.
                Accept::Clone | Accept::UpdatePlugin => unreachable!(),
            }
        }
    }
}

fn open_kind(accept: Accept) -> Option<&'static str> {
    match accept {
        Accept::Workspace => Some("workspace"),
        Accept::Tab => Some("tab"),
        Accept::Split => Some("split"),
        Accept::Pane => Some("pane"),
        _ => None,
    }
}

fn herdr(runner: &dyn CommandRunner, args: &[&str]) -> Result<()> {
    if runner.ok("herdr", args) {
        Ok(())
    } else {
        Err(anyhow!("herdr {} failed", args.join(" ")))
    }
}

/// The `open` subcommand's worker. `bin/get.sh` (the clone flow) calls
/// `herdr-switchboard open …` instead of re-implementing the herdr verbs in
/// bash, so a change to how a target opens lands in exactly one place. Split
/// geometry comes from `cfg`, the same as the picker's own opens.
pub fn open_target(
    runner: &dyn CommandRunner,
    target: &str,
    path: &str,
    origin: &str,
    label: &str,
    cfg: &Config,
) -> Result<()> {
    open_repo(runner, target, path, origin, label, cfg)
}

fn open_repo(
    runner: &dyn CommandRunner,
    target: &str,
    path: &str,
    origin: &str,
    label: &str,
    cfg: &Config,
) -> Result<()> {
    if !std::path::Path::new(path).is_dir() {
        return Err(anyhow!("path no longer exists: {path}"));
    }
    match target {
        "workspace" => herdr(
            runner,
            &[
                "workspace",
                "create",
                "--cwd",
                path,
                "--label",
                label,
                "--focus",
            ],
        ),
        "tab" => herdr(
            runner,
            &["tab", "create", "--cwd", path, "--label", label, "--focus"],
        ),
        "split" => {
            let dir = cfg.projects.split_direction.clone();
            let ratio = cfg.projects.split_ratio.clone();
            let mut args = vec!["pane", "split"];
            if !origin.is_empty() {
                args.push(origin);
            }
            args.extend_from_slice(&[
                "--direction",
                &dir,
                "--ratio",
                &ratio,
                "--cwd",
                path,
                "--focus",
            ]);
            herdr(runner, &args)
        }
        "pane" => {
            if origin.is_empty() {
                return Err(anyhow!("no origin pane to cd into"));
            }
            herdr(
                runner,
                &["pane", "send-text", origin, &format!("cd '{path}'")],
            )?;
            herdr(runner, &["pane", "send-keys", origin, "enter"])
        }
        other => Err(anyhow!("unknown target {other}")),
    }
}

fn focus_workspace(runner: &dyn CommandRunner, id: &str) -> Result<()> {
    herdr(runner, &["workspace", "focus", id])
}

fn focus_agent(runner: &dyn CommandRunner, id: &str) -> Result<()> {
    herdr(runner, &["agent", "focus", id])
}

/// Hand the whole terminal to `bin/review.sh` with the git menu's resolved choice
/// as environment, replacing this process in the git pane the way the clone flow
/// `exec`s `get.sh`. `review.sh` maps `REVIEW_MODE` to the tool (`tuicr` review,
/// `lazygit` staging, or a custom `menu.conf` command).
pub fn run_review(spec: &ReviewSpec, script_dir: &str) -> Result<()> {
    let err = Command::new("bash")
        .arg(format!("{script_dir}/review.sh"))
        .env("REVIEW_MODE", &spec.mode)
        .env("REVIEW_CWD", &spec.cwd)
        .env("REVIEW_ARG", &spec.arg)
        .env("REVIEW_CUSTOM", &spec.custom)
        .env("REVIEW_LABEL", &spec.label)
        .exec();
    Err(anyhow!("failed to exec review.sh: {err}"))
}

fn update(runner: &dyn CommandRunner, rel: &str, label: &str) -> Result<()> {
    println!("\x1b[1mUpdating\x1b[0m {rel}\n");
    let _ = runner.status("ghq", &["get", "-u", "--", rel]);
    println!("\n\x1b[2m{label}: press Enter to close\x1b[0m");
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
    Ok(())
}

fn remove(runner: &dyn CommandRunner, path: &str, label: &str) -> Result<()> {
    println!("\x1b[1;31mRemove repository\x1b[0m\n  {path}\n");
    print!("Type the repo name (\x1b[1m{label}\x1b[0m) to confirm: ");
    io::stdout().flush().ok();
    let mut reply = String::new();
    io::stdin().read_line(&mut reply)?;
    if reply.trim() == label {
        runner.status("rm", &["-rf", "--", path])?;
        println!("Removed {label}.");
    } else {
        println!("Aborted.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;
    use ratatui::style::Color;

    fn repo_entry(dir: &str) -> Entry {
        Entry {
            kind: Kind::Repo,
            id: "o/r".into(),
            dir: Some(dir.to_string()),
            label: "r".into(),
            icon: String::new(),
            icon_color: Color::Reset,
            primary: String::new(),
            secondary: String::new(),
            search: String::new(),
        }
    }

    fn worktree_entry(dir: &str) -> Entry {
        Entry {
            kind: Kind::Worktree,
            id: dir.into(),
            dir: Some(dir.into()),
            label: "feature-auth".into(),
            icon: String::new(),
            icon_color: Color::Reset,
            primary: "o/r".into(),
            secondary: "feature/auth".into(),
            search: String::new(),
        }
    }

    /// A throwaway real directory: `open_repo` refuses a path that is not one.
    fn tmp_repo(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ghq-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dispatch_tab_builds_the_herdr_tab_create_verb() {
        let dir = tmp_repo("tab");
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new();
        dispatch(
            &runner,
            Some(repo_entry(&path)),
            Accept::Tab,
            "",
            &Config::default(),
            ".",
            "workspace",
        )
        .unwrap();
        assert_eq!(
            runner.calls()[0],
            vec!["herdr", "tab", "create", "--cwd", &path, "--label", "r", "--focus"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dispatch_worktree_opens_its_linked_path() {
        let dir = tmp_repo("linked-worktree");
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new().on("herdr tab create", "");
        dispatch(
            &runner,
            Some(worktree_entry(&path)),
            Accept::Tab,
            "pane-1",
            &Config::default(),
            ".",
            "workspace",
        )
        .unwrap();
        assert_eq!(
            runner.calls()[0],
            vec![
                "herdr",
                "tab",
                "create",
                "--cwd",
                &path,
                "--label",
                "feature-auth",
                "--focus"
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dispatch_worktree_rejects_repo_update_and_remove() {
        let runner = MockRunner::new();
        for accept in [Accept::Update, Accept::Remove] {
            let err = dispatch(
                &runner,
                Some(worktree_entry("/linked")),
                accept,
                "",
                &Config::default(),
                ".",
                "workspace",
            )
            .unwrap_err();
            assert!(err.to_string().contains("not supported for worktrees"));
        }
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn dispatch_pane_sends_cd_to_the_captured_origin() {
        let dir = tmp_repo("pane");
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new();
        dispatch(
            &runner,
            Some(repo_entry(&path)),
            Accept::Pane,
            "pane-9",
            &Config::default(),
            ".",
            "workspace",
        )
        .unwrap();
        let calls = runner.calls();
        assert_eq!(
            calls[0],
            vec![
                "herdr",
                "pane",
                "send-text",
                "pane-9",
                &format!("cd '{path}'")
            ]
        );
        assert_eq!(
            calls[1],
            vec!["herdr", "pane", "send-keys", "pane-9", "enter"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dispatch_default_repo_uses_the_resolved_default_target() {
        let dir = tmp_repo("def");
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new();
        // No force, config says "tab": Enter on a repo lands it in a tab.
        dispatch(
            &runner,
            Some(repo_entry(&path)),
            Accept::Default,
            "",
            &Config::default(),
            ".",
            "tab",
        )
        .unwrap();
        assert_eq!(runner.calls()[0][..3], ["herdr", "tab", "create"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dispatch_propagates_a_herdr_failure() {
        let dir = tmp_repo("fail");
        let path = dir.to_string_lossy().to_string();
        // herdr exits non-zero: the open must surface an error, not swallow it.
        let runner = MockRunner::new().failing("tab create");
        let res = dispatch(
            &runner,
            Some(repo_entry(&path)),
            Accept::Tab,
            "",
            &Config::default(),
            ".",
            "workspace",
        );
        assert!(res.is_err(), "a failing herdr verb must not report success");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dispatch_a_workspace_entry_focuses_it_without_a_path() {
        let entry = Entry {
            kind: Kind::Workspace,
            id: "ws-3".into(),
            dir: None,
            label: "work".into(),
            icon: String::new(),
            icon_color: Color::Reset,
            primary: String::new(),
            secondary: String::new(),
            search: String::new(),
        };
        let runner = MockRunner::new();
        dispatch(
            &runner,
            Some(entry),
            Accept::Default,
            "",
            &Config::default(),
            ".",
            "workspace",
        )
        .unwrap();
        assert_eq!(
            runner.calls()[0],
            vec!["herdr", "workspace", "focus", "ws-3"]
        );
    }

    #[test]
    fn forced_target_overrides_the_configured_default() {
        assert_eq!(resolve_default_target(Some("tab"), "workspace"), "tab");
        assert_eq!(resolve_default_target(Some("split"), "pane"), "split");
    }

    /// The env var is the whole contract with `bin/action.sh`. Sole test that
    /// touches `SWITCHBOARD_FORCE_TARGET`, so the process-global set/remove is safe.
    #[test]
    fn forced_target_reads_the_env_var_action_sh_sets() {
        env::set_var("SWITCHBOARD_FORCE_TARGET", "tab");
        assert_eq!(forced_target().as_deref(), Some("tab"));
        assert_eq!(
            resolve_default_target(forced_target().as_deref(), "workspace"),
            "tab"
        );

        // `menu` opens the pane without the var: the config must win.
        env::remove_var("SWITCHBOARD_FORCE_TARGET");
        assert_eq!(forced_target(), None);
        assert_eq!(
            resolve_default_target(forced_target().as_deref(), "workspace"),
            "workspace"
        );

        // action.sh passes `--env SWITCHBOARD_FORCE_TARGET=` when force_target is empty.
        env::set_var("SWITCHBOARD_FORCE_TARGET", "");
        assert_eq!(forced_target(), None);
        env::remove_var("SWITCHBOARD_FORCE_TARGET");
    }

    #[test]
    fn configured_default_applies_when_nothing_is_forced() {
        assert_eq!(resolve_default_target(None, "pane"), "pane");
    }

    #[test]
    fn unrecognised_values_fall_back_to_workspace() {
        // A bad force never breaks the open; it defers to the config.
        assert_eq!(resolve_default_target(Some("bogus"), "tab"), "tab");
        // A bad config with no force lands on the documented default.
        assert_eq!(resolve_default_target(None, "bogus"), "workspace");
        assert_eq!(resolve_default_target(Some("bogus"), "bogus"), "workspace");
        // An empty force is the unset case (`forced_target` filters it out).
        assert_eq!(resolve_default_target(None, ""), "workspace");
    }
}
