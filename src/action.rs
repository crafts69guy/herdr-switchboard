//! Accept dispatch — runs AFTER the TUI is torn down, so interactive bits
//! (clone prompt, remove confirm, update output) use the normal pane.

use std::env;
use std::io::{self, BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::data::{Config, Entry, Kind};
use crate::fnm::{self, Preparation};
use crate::git::ReviewSpec;
use crate::notify::{Event as NotifyEvent, Notifier};
use crate::runner::CommandRunner;
use anyhow::{anyhow, Result};

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

/// Whether a restored-terminal action ran or the user explicitly cancelled it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchOutcome {
    Completed,
    Aborted,
}

pub fn dispatch(
    runner: &dyn CommandRunner,
    entry: Option<Entry>,
    accept: Accept,
    origin_pane: &str,
    cfg: &Config,
    script_dir: &str,
    default_target: &str,
) -> Result<DispatchOutcome> {
    dispatch_with(
        replace_process,
        runner,
        entry,
        accept,
        origin_pane,
        cfg,
        script_dir,
        default_target,
    )
}

/// Replace this process with `command`. Returns only when the exec failed.
fn replace_process(command: &mut Command) -> io::Error {
    command.exec()
}

/// [`dispatch`] with the process replacement passed in: a test cannot let the
/// clone or update flow `exec` over the test runner, so it hands in one that
/// reports a failure instead.
#[allow(clippy::too_many_arguments)]
fn dispatch_with(
    exec: fn(&mut Command) -> io::Error,
    runner: &dyn CommandRunner,
    entry: Option<Entry>,
    accept: Accept,
    origin_pane: &str,
    cfg: &Config,
    script_dir: &str,
    default_target: &str,
) -> Result<DispatchOutcome> {
    if accept == Accept::Clone {
        // Hand the whole terminal to the bash clone flow.
        let err = exec(Command::new("bash").arg(format!("{script_dir}/get.sh")));
        return Err(anyhow!("failed to exec get.sh: {err}"));
    }

    // Must replace this process, not spawn beside it: `herdr plugin install` rewrites
    // the checkout holding the very binary running here. Needs no selection either.
    if accept == Accept::UpdatePlugin {
        let err = exec(Command::new("bash").arg(format!("{script_dir}/update-plugin.sh")));
        return Err(anyhow!("failed to exec update-plugin.sh: {err}"));
    }

    let e = entry.ok_or_else(|| anyhow!("no selection"))?;

    match e.kind {
        Kind::Workspace => focus_workspace(runner, &e.id).map(|_| DispatchOutcome::Completed),
        Kind::Agent => {
            let target = open_kind(accept);
            match (target, e.dir.as_deref()) {
                (Some(t), Some(dir)) => open_repo(runner, t, dir, origin_pane, &e.label, cfg),
                _ => focus_agent(runner, &e.id),
            }
            .map(|_| DispatchOutcome::Completed)
        }
        Kind::Repo | Kind::Worktree => {
            let dir = e.dir.clone().unwrap_or_default();
            match accept {
                Accept::Default => {
                    open_repo(runner, default_target, &dir, origin_pane, &e.label, cfg)
                        .map(|_| DispatchOutcome::Completed)
                }
                Accept::Workspace => {
                    open_repo(runner, "workspace", &dir, origin_pane, &e.label, cfg)
                        .map(|_| DispatchOutcome::Completed)
                }
                Accept::Tab => open_repo(runner, "tab", &dir, origin_pane, &e.label, cfg)
                    .map(|_| DispatchOutcome::Completed),
                Accept::Split => open_repo(runner, "split", &dir, origin_pane, &e.label, cfg)
                    .map(|_| DispatchOutcome::Completed),
                Accept::Pane => open_repo(runner, "pane", &dir, origin_pane, &e.label, cfg)
                    .map(|_| DispatchOutcome::Completed),
                Accept::Update if e.kind == Kind::Repo => {
                    update(runner, &e.id, &e.label).map(|_| DispatchOutcome::Completed)
                }
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
    let preparation = if target != "pane" && cfg.fnm.enabled {
        fnm::prepare(runner, path)
    } else {
        Preparation::Unmanaged
    };
    let result = match target {
        "workspace" => open_new_target(
            runner,
            vec![
                "workspace".into(),
                "create".into(),
                "--cwd".into(),
                path.into(),
                "--label".into(),
                label.into(),
                "--focus".into(),
            ],
            &preparation,
        ),
        "tab" => open_new_target(
            runner,
            vec![
                "tab".into(),
                "create".into(),
                "--cwd".into(),
                path.into(),
                "--label".into(),
                label.into(),
                "--focus".into(),
            ],
            &preparation,
        ),
        "split" => {
            let dir = cfg.projects.split_direction.clone();
            let ratio = cfg.projects.split_ratio.clone();
            let mut args = vec!["pane".into(), "split".into()];
            if !origin.is_empty() {
                args.push(origin.into());
            }
            args.extend([
                "--direction".into(),
                dir,
                "--ratio".into(),
                ratio,
                "--cwd".into(),
                path.into(),
                "--focus".into(),
            ]);
            open_new_target(runner, args, &preparation)
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
    };
    if result.is_ok() && preparation == Preparation::Unavailable {
        Notifier::new(cfg).send(NotifyEvent::FnmActivationFailed, None);
    }
    result
}

/// Create a fresh terminal with fnm's resolved PATH. Shell startup owns any
/// session-local initialization; target creation never injects terminal input.
fn open_new_target(
    runner: &dyn CommandRunner,
    mut args: Vec<String>,
    preparation: &Preparation,
) -> Result<()> {
    if let Preparation::Ready { path } = preparation {
        args.extend(["--env".into(), format!("PATH={path}")]);
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    herdr(runner, &refs)
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
    run_review_with(replace_process, spec, script_dir)
}

fn run_review_with(
    exec: fn(&mut Command) -> io::Error,
    spec: &ReviewSpec,
    script_dir: &str,
) -> Result<()> {
    let err = exec(
        Command::new("bash")
            .arg(format!("{script_dir}/review.sh"))
            .env("REVIEW_MODE", &spec.mode)
            .env("REVIEW_CWD", &spec.cwd)
            .env("REVIEW_ARG", &spec.arg)
            .env("REVIEW_CUSTOM", &spec.custom)
            .env("REVIEW_LABEL", &spec.label),
    );
    Err(anyhow!("failed to exec review.sh: {err}"))
}

fn update(runner: &dyn CommandRunner, rel: &str, label: &str) -> Result<()> {
    update_with(
        runner,
        rel,
        label,
        &mut io::stdin().lock(),
        &mut io::stdout(),
    )
}

/// `ghq get -u` on the restored terminal, then wait for Enter so its output can
/// be read before the pane closes.
fn update_with(
    runner: &dyn CommandRunner,
    rel: &str,
    label: &str,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<()> {
    writeln!(output, "\x1b[1mUpdating\x1b[0m {rel}\n")?;
    let _ = runner.status("ghq", &["get", "-u", "--", rel]);
    writeln!(output, "\n\x1b[2m{label}: press Enter to close\x1b[0m")?;
    let mut s = String::new();
    let _ = input.read_line(&mut s);
    Ok(())
}

fn remove(runner: &dyn CommandRunner, path: &str, label: &str) -> Result<DispatchOutcome> {
    remove_with(
        runner,
        path,
        label,
        &mut io::stdin().lock(),
        &mut io::stdout(),
    )
}

/// The removal prompt: the repository name typed back is the only confirmation.
fn remove_with(
    runner: &dyn CommandRunner,
    path: &str,
    label: &str,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<DispatchOutcome> {
    writeln!(output, "\x1b[1;31mRemove repository\x1b[0m\n  {path}\n")?;
    write!(
        output,
        "Type the repo name (\x1b[1m{label}\x1b[0m) to confirm: "
    )?;
    output.flush().ok();
    let mut reply = String::new();
    input.read_line(&mut reply)?;
    let outcome = remove_with_confirmation(runner, path, label, reply.trim())?;
    match outcome {
        DispatchOutcome::Completed => writeln!(output, "Removed {label}.")?,
        DispatchOutcome::Aborted => writeln!(output, "Aborted.")?,
    }
    Ok(outcome)
}

fn remove_with_confirmation(
    runner: &dyn CommandRunner,
    path: &str,
    label: &str,
    confirmation: &str,
) -> Result<DispatchOutcome> {
    if confirmation != label {
        return Ok(DispatchOutcome::Aborted);
    }
    anyhow::ensure!(
        runner.ok("rm", &["-rf", "--", path]),
        "remove repository failed"
    );
    Ok(DispatchOutcome::Completed)
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

    fn fnm_config() -> Config {
        let mut cfg = Config::default();
        cfg.fnm.enabled = true;
        cfg.common.notifications = false;
        cfg
    }

    /// A directory that exists, so the open is not refused before it starts.
    fn repo_dir(tag: &str) -> std::path::PathBuf {
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "switchboard-open-{tag}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `open_target` is the seam `bin/get.sh` calls after a clone, and it must
    /// build the same herdr verbs the picker's own opens do — that is the whole
    /// point of it not being reimplemented in bash.
    #[test]
    fn the_clone_flow_opens_a_repo_through_the_same_herdr_verbs() {
        for (target, verb) in [
            ("workspace", "workspace"),
            ("tab", "tab"),
            ("split", "pane"),
        ] {
            let dir = repo_dir(target);
            let path = dir.to_string_lossy().into_owned();
            let runner = MockRunner::new();
            open_target(&runner, target, &path, "w1:p1", "api", &Config::default()).unwrap();

            let calls = runner.calls();
            assert!(
                calls
                    .iter()
                    .any(|argv| argv[0] == "herdr" && argv.contains(&verb.to_string())),
                "`{target}` did not reach `herdr {verb}`: {calls:?}"
            );
            assert!(
                calls.iter().any(|argv| argv.iter().any(|arg| arg == &path)),
                "`{target}` did not carry the path: {calls:?}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// A repository that has been deleted since the catalogue was built is
    /// refused before any herdr verb runs — opening a workspace onto a path
    /// that is gone leaves an empty pane with no explanation.
    #[test]
    fn opening_a_path_that_no_longer_exists_is_refused_before_herdr_is_called() {
        let runner = MockRunner::new();
        let error = open_target(
            &runner,
            "workspace",
            "/definitely/not/a/real/path",
            "w1:p1",
            "api",
            &Config::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("no longer exists"), "{error}");
        assert!(runner.calls().is_empty(), "herdr was called anyway");
    }

    /// A failed herdr verb is reported rather than treated as an open.
    #[test]
    fn a_failed_open_is_reported() {
        let dir = repo_dir("failing");
        let runner = MockRunner::new().failing("herdr");
        let error = open_target(
            &runner,
            "workspace",
            &dir.to_string_lossy(),
            "w1:p1",
            "api",
            &Config::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("herdr"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The current pane is a `cd`, not a new surface: opening "here" must not
    /// create a workspace, tab or pane.
    #[test]
    fn opening_in_the_current_pane_only_sends_a_cd() {
        let dir = repo_dir("pane");
        let runner = MockRunner::new();
        open_target(
            &runner,
            "pane",
            &dir.to_string_lossy(),
            "w1:p1",
            "api",
            &Config::default(),
        )
        .unwrap();

        let calls = runner.calls();
        assert!(
            !calls
                .iter()
                .any(|argv| argv.contains(&"create".to_string())),
            "opening in place created a surface: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|argv| argv.iter().any(|arg| arg.contains("cd "))),
            "no cd was sent: {calls:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An unrecognised target on the `open` subcommand is refused by name.
    ///
    /// Note the deliberate asymmetry with `resolve_default_target`, which
    /// degrades an unknown *config* value to `workspace`: a config written for a
    /// later version must not stop the picker opening, but an explicit CLI
    /// argument that means nothing is a caller bug, and opening somewhere
    /// arbitrary would hide it.
    #[test]
    fn an_unrecognised_open_target_is_refused_rather_than_guessed() {
        let dir = repo_dir("unknown");
        let runner = MockRunner::new();
        let error = open_target(
            &runner,
            "nonsense",
            &dir.to_string_lossy(),
            "w1:p1",
            "api",
            &Config::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown target"), "{error}");
        assert!(runner.calls().is_empty(), "it opened something anyway");
        assert_eq!(
            resolve_default_target(None, "nonsense"),
            "workspace",
            "a config value degrades where a CLI argument errors"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A worktree is openable but not a repository to update or remove: those
    /// verbs have different semantics on a linked checkout, so they are refused
    /// rather than run against the wrong thing.
    #[test]
    fn every_accept_either_opens_or_is_local_work() {
        // The four that open name a surface; the rest do local work with no
        // surface of their own. Exhaustive on purpose: a new variant that
        // belongs in neither group is a dispatch arm somebody forgot.
        let opens = [Accept::Workspace, Accept::Tab, Accept::Split, Accept::Pane];
        let local = [
            Accept::Default,
            Accept::Update,
            Accept::Remove,
            Accept::Clone,
            Accept::UpdatePlugin,
        ];
        assert_eq!(
            opens.len() + local.len(),
            9,
            "a variant was added or removed"
        );
        assert!(opens.iter().all(|a| open_kind(*a).is_some()));
        assert!(local.iter().all(|a| open_kind(*a).is_none()));
    }

    /// Only the four opening accepts name a herdr surface; the rest are local
    /// work with no surface of their own.
    #[test]
    fn only_the_opening_accepts_name_a_herdr_surface() {
        assert_eq!(open_kind(Accept::Workspace), Some("workspace"));
        assert_eq!(open_kind(Accept::Tab), Some("tab"));
        assert_eq!(open_kind(Accept::Split), Some("split"));
        assert_eq!(open_kind(Accept::Pane), Some("pane"));

        for accept in [
            Accept::Default,
            Accept::Update,
            Accept::Remove,
            Accept::Clone,
            Accept::UpdatePlugin,
        ] {
            assert_eq!(open_kind(accept), None, "{accept:?} is not an open");
        }
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
    fn fnm_workspace_open_passes_path_without_terminal_input() {
        let dir = tmp_repo("fnm-workspace");
        std::fs::write(dir.join(".nvmrc"), "22\n").unwrap();
        let path = dir.to_string_lossy().to_string();
        let response = r#"{"result":{"root_pane":{"pane_id":"pane-new"}}}"#;
        let runner = MockRunner::new()
            .on("fnm exec", "/fnm/v22/bin:/usr/bin\n")
            .on("workspace create", response);

        dispatch(
            &runner,
            Some(repo_entry(&path)),
            Accept::Workspace,
            "pane-origin",
            &fnm_config(),
            ".",
            "workspace",
        )
        .unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            vec![
                "herdr",
                "workspace",
                "create",
                "--cwd",
                &path,
                "--label",
                "r",
                "--focus",
                "--env",
                "PATH=/fnm/v22/bin:/usr/bin"
            ]
        );
        assert!(!calls
            .iter()
            .any(|call| call.windows(2).any(|pair| pair == ["pane", "run"])));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn fnm_tab_and_split_pass_path_without_terminal_input() {
        for (tag, accept, response, expected_prefix) in [
            (
                "fnm-tab",
                Accept::Tab,
                r#"{"result":{"root_pane":{"pane_id":"pane-tab"}}}"#,
                ["herdr", "tab", "create"],
            ),
            (
                "fnm-split",
                Accept::Split,
                r#"{"result":{"pane":{"pane_id":"pane-split"}}}"#,
                ["herdr", "pane", "split"],
            ),
        ] {
            let dir = tmp_repo(tag);
            std::fs::write(dir.join(".node-version"), "22\n").unwrap();
            let path = dir.to_string_lossy().to_string();
            let runner = MockRunner::new()
                .on("fnm exec", "/fnm/v22/bin:/usr/bin\n")
                .on(&expected_prefix[1..].join(" "), response);

            dispatch(
                &runner,
                Some(repo_entry(&path)),
                accept,
                "pane-origin",
                &fnm_config(),
                ".",
                "workspace",
            )
            .unwrap();

            let calls = runner.calls();
            assert_eq!(calls.len(), 2);
            assert_eq!(&calls[1][..3], expected_prefix);
            assert!(calls[1]
                .windows(2)
                .any(|pair| pair == ["--env", "PATH=/fnm/v22/bin:/usr/bin"]));
            assert!(!calls
                .iter()
                .any(|call| call.windows(2).any(|pair| pair == ["pane", "run"])));
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn fnm_current_pane_keeps_the_cd_only_activation_contract() {
        let dir = tmp_repo("fnm-pane");
        std::fs::write(dir.join(".nvmrc"), "22\n").unwrap();
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new();

        dispatch(
            &runner,
            Some(repo_entry(&path)),
            Accept::Pane,
            "pane-9",
            &fnm_config(),
            ".",
            "workspace",
        )
        .unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][..3], ["herdr", "pane", "send-text"]);
        assert_eq!(
            calls[1],
            vec!["herdr", "pane", "send-keys", "pane-9", "enter"]
        );
        assert!(!calls.iter().any(|call| call[0] == "fnm"));
        assert!(!calls
            .iter()
            .any(|call| call.windows(2).any(|pair| pair == ["pane", "run"])));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unavailable_fnm_version_does_not_block_the_open_or_add_an_env() {
        let dir = tmp_repo("fnm-unavailable");
        std::fs::write(dir.join(".nvmrc"), "999\n").unwrap();
        let path = dir.to_string_lossy().to_string();
        let runner = MockRunner::new().failing("fnm exec");

        dispatch(
            &runner,
            Some(repo_entry(&path)),
            Accept::Tab,
            "pane-origin",
            &fnm_config(),
            ".",
            "workspace",
        )
        .unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1][..3], ["herdr", "tab", "create"]);
        assert!(!calls[1].iter().any(|arg| arg == "--env"));
        std::fs::remove_dir_all(dir).ok();
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
    fn removal_confirmation_reports_abort_completion_and_command_failure() {
        let aborted = MockRunner::new();
        assert_eq!(
            remove_with_confirmation(&aborted, "/repo", "repo", "wrong").unwrap(),
            DispatchOutcome::Aborted
        );
        assert!(aborted.calls().is_empty());

        let removed = MockRunner::new();
        assert_eq!(
            remove_with_confirmation(&removed, "/repo", "repo", "repo").unwrap(),
            DispatchOutcome::Completed
        );
        assert_eq!(removed.calls()[0], vec!["rm", "-rf", "--", "/repo"]);

        let failed = MockRunner::new().failing("rm -rf");
        let error = remove_with_confirmation(&failed, "/repo", "repo", "repo").unwrap_err();
        assert!(error.to_string().contains("remove repository failed"));
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

    /// Update runs `ghq get -u` and waits for Enter before closing.
    #[test]
    fn update_fetches_with_ghq_then_waits_for_enter() {
        let runner = MockRunner::new();
        let mut shown = Vec::new();
        update_with(
            &runner,
            "github.com/o/r",
            "r",
            &mut "\n".as_bytes(),
            &mut shown,
        )
        .unwrap();
        assert_eq!(
            runner.calls(),
            vec![vec!["ghq", "get", "-u", "--", "github.com/o/r"]]
        );
        assert!(String::from_utf8(shown).unwrap().contains("press Enter"));
    }

    /// Removal deletes only when the typed name matches, and says which.
    #[test]
    fn removal_deletes_only_on_the_typed_name() {
        let runner = MockRunner::new();
        let mut shown = Vec::new();
        let outcome =
            remove_with(&runner, "/src/o/r", "r", &mut "r\n".as_bytes(), &mut shown).unwrap();
        assert_eq!(outcome, DispatchOutcome::Completed);
        assert!(String::from_utf8(shown).unwrap().contains("Removed r."));
        assert_eq!(runner.calls(), vec![vec!["rm", "-rf", "--", "/src/o/r"]]);

        let runner = MockRunner::new();
        let mut shown = Vec::new();
        let outcome = remove_with(
            &runner,
            "/src/o/r",
            "r",
            &mut "nope\n".as_bytes(),
            &mut shown,
        )
        .unwrap();
        assert_eq!(outcome, DispatchOutcome::Aborted);
        assert!(String::from_utf8(shown).unwrap().contains("Aborted."));
        assert!(runner.calls().is_empty());
    }

    fn live_entry(kind: Kind, id: &str, dir: Option<&str>) -> Entry {
        Entry {
            kind,
            id: id.into(),
            dir: dir.map(str::to_string),
            ..repo_entry("/unused")
        }
    }

    /// Live rows focus what they are; an agent opened into a target lands in
    /// its own cwd; worktrees refuse repo-only verbs; nothing selected is an error.
    #[test]
    fn dispatch_focuses_live_rows_and_refuses_what_does_not_apply() {
        let cfg = Config::default();
        let runner = MockRunner::new();
        let go = |entry: Option<Entry>, accept: Accept| {
            dispatch(&runner, entry, accept, "w1:p1", &cfg, "/bin", "workspace")
        };
        go(
            Some(live_entry(Kind::Workspace, "w2", None)),
            Accept::Default,
        )
        .unwrap();
        go(
            Some(live_entry(Kind::Agent, "w1:p3", None)),
            Accept::Default,
        )
        .unwrap();
        let dir = std::env::temp_dir();
        go(
            Some(live_entry(
                Kind::Agent,
                "w1:p3",
                Some(dir.to_str().unwrap()),
            )),
            Accept::Tab,
        )
        .unwrap();
        let calls = runner.calls();
        assert_eq!(calls[0], ["herdr", "workspace", "focus", "w2"]);
        assert_eq!(calls[1], ["herdr", "agent", "focus", "w1:p3"]);
        assert_eq!(calls[2][..3], ["herdr", "tab", "create"]);

        let worktree = live_entry(Kind::Worktree, "/wt", Some(dir.to_str().unwrap()));
        assert!(go(Some(worktree.clone()), Accept::Update).is_err());
        assert!(go(Some(worktree), Accept::Remove).is_err());
        assert!(go(None, Accept::Default).is_err());
    }

    #[test]
    fn opening_in_this_pane_needs_an_origin_and_a_known_target() {
        let cfg = Config::default();
        let runner = MockRunner::new();
        let dir = std::env::temp_dir();
        let dir = dir.to_str().unwrap();
        assert!(open_target(&runner, "pane", dir, "", "r", &cfg).is_err());
        assert!(open_target(&runner, "sideways", dir, "w1:p1", "r", &cfg).is_err());
    }

    /// Clone, the plugin update, and a review replace the process with a Bash
    /// flow; here the replacement is refused, which is the only way it returns,
    /// and each says which script it could not start.
    #[test]
    fn a_refused_process_replacement_names_its_script() {
        fn refuse(command: &mut Command) -> io::Error {
            assert_eq!(command.get_program(), "bash");
            io::Error::other("exec refused in tests")
        }
        let cfg = Config::default();
        let runner = MockRunner::new();
        for (accept, script) in [
            (Accept::Clone, "get.sh"),
            (Accept::UpdatePlugin, "update-plugin.sh"),
        ] {
            let error =
                dispatch_with(refuse, &runner, None, accept, "", &cfg, "/bin", "tab").unwrap_err();
            assert!(error.to_string().contains(script), "{error}");
        }
        let spec = ReviewSpec {
            mode: "worktree".into(),
            cwd: "/repo".into(),
            arg: String::new(),
            custom: String::new(),
            label: "repo".into(),
        };
        let error = run_review_with(refuse, &spec, "/bin").unwrap_err();
        assert!(error.to_string().contains("review.sh"), "{error}");
        assert!(runner.calls().is_empty());
    }
}
