//! Interactive fnm version manager.
//!
//! Local versions are loaded before the first frame. Remote versions cross a
//! background effect seam because `fnm list-remote` performs network work.
//! Mutations run only after the picker has restored the terminal.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};

use crate::config::Config;
use crate::data::Theme;
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema, MatchKind};
use crate::runner::{CommandRunner, SystemRunner};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Source {
    Installed,
    Remote,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Version {
    value: String,
    source: Source,
    current: bool,
    default: bool,
}

impl Version {
    fn id(&self) -> String {
        let prefix = match self.source {
            Source::Installed => "installed",
            Source::Remote => "remote",
        };
        format!("{prefix}:{}", self.value)
    }
}

enum RemoteResult {
    Loaded(Vec<Version>),
    Failed(String),
}

struct FnmMode {
    installed: Vec<Version>,
    remote: Vec<Version>,
    remote_error: Option<String>,
    remote_rx: Option<Receiver<RemoteResult>>,
    origin_pane: String,
}

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    picker::run(FnmMode::new(), theme, cfg)
}

impl FnmMode {
    fn new() -> Self {
        Self {
            installed: Vec::new(),
            remote: Vec::new(),
            remote_error: None,
            remote_rx: None,
            origin_pane: std::env::var("SWITCHBOARD_ORIGIN_PANE_ID").unwrap_or_default(),
        }
    }

    fn start_remote_load(&mut self) {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = load_remote(&SystemRunner)
                .map(RemoteResult::Loaded)
                .unwrap_or_else(|error| RemoteResult::Failed(error.to_string()));
            let _ = sender.send(result);
        });
        self.remote_rx = Some(receiver);
    }

    fn items(&self) -> Vec<PickerItem> {
        let installed = self.installed.iter().map(version_item).collect::<Vec<_>>();
        let installed_values = self
            .installed
            .iter()
            .map(|version| version.value.as_str())
            .collect::<HashSet<_>>();
        let mut items = installed;
        items.extend(
            self.remote
                .iter()
                .filter(|version| !installed_values.contains(version.value.as_str()))
                .map(version_item),
        );
        if self.remote_rx.is_some() {
            items.push(status_item(
                "remote-loading",
                "Loading remote versions…",
                "fnm list-remote is running in the background",
            ));
        } else if let Some(error) = &self.remote_error {
            items.push(status_item(
                "remote-error",
                "Remote versions unavailable",
                error,
            ));
        }
        items
    }

    fn find(&self, id: &str) -> Option<&Version> {
        self.installed
            .iter()
            .chain(self.remote.iter())
            .find(|version| version.id() == id)
    }
}

impl PickerMode for FnmMode {
    fn title(&self) -> &str {
        "Node Versions"
    }

    fn accent_slot(&self) -> &'static str {
        "green"
    }

    fn list_pct(&self) -> u16 {
        52
    }

    fn schema(&self) -> FieldSchema {
        FieldSchema::new(
            &[
                ("version", MatchKind::Contains),
                ("source", MatchKind::Exact),
                ("status", MatchKind::Exact),
            ],
            &[("v", "version")],
        )
    }

    fn actions(&self) -> Vec<ActionSpec> {
        vec![
            ActionSpec {
                id: "primary",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                key_label: "↵".into(),
                label: "use/install",
                color_slot: "green",
            },
            ActionSpec {
                id: "use",
                key: KeyCode::Char('u'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥u".into(),
                label: "use",
                color_slot: "blue",
            },
            ActionSpec {
                id: "install",
                key: KeyCode::Char('i'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥i".into(),
                label: "install",
                color_slot: "green",
            },
            ActionSpec {
                id: "default",
                key: KeyCode::Char('d'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥d".into(),
                label: "default",
                color_slot: "peach",
            },
            ActionSpec {
                id: "uninstall",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥x".into(),
                label: "uninstall",
                color_slot: "red",
            },
            ActionSpec {
                id: "refresh",
                key: KeyCode::Char('r'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥r".into(),
                label: "refresh",
                color_slot: "teal",
            },
        ]
    }

    fn action_disabled_reason(&self, item_id: &str, action: &str) -> Option<String> {
        if action == "refresh" {
            return None;
        }
        let Some(version) = self.find(item_id) else {
            return Some("wait for remote versions or refresh the list".into());
        };
        match action {
            "primary" => None,
            "use" | "default" if version.source != Source::Installed => {
                Some("install this version first".into())
            }
            "install" if version.source != Source::Remote => {
                Some("this version is already installed".into())
            }
            "uninstall" if version.source != Source::Installed => {
                Some("only installed versions can be removed".into())
            }
            "uninstall" if version.value == "system" => {
                Some("the system Node installation is not managed by fnm".into())
            }
            _ => None,
        }
    }

    fn initial(&mut self) -> Result<Vec<PickerItem>> {
        self.installed = load_installed(&SystemRunner)?;
        self.remote.clear();
        self.remote_error = None;
        self.start_remote_load();
        Ok(self.items())
    }

    fn poll(&mut self) -> Option<Result<Vec<PickerItem>>> {
        let receiver = self.remote_rx.as_ref()?;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                RemoteResult::Failed("fnm list-remote stopped unexpectedly".into())
            }
        };
        self.remote_rx = None;
        match result {
            RemoteResult::Loaded(versions) => self.remote = versions,
            RemoteResult::Failed(error) => self.remote_error = Some(error),
        }
        Some(Ok(self.items()))
    }

    fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome> {
        if action == "refresh" {
            return Ok(ActionOutcome::StayOpen);
        }
        let version = self
            .find(item_id)
            .cloned()
            .context("selected fnm version is no longer available")?;
        let operation = match action {
            "primary" if version.source == Source::Installed => Operation::Use,
            "primary" => Operation::Install,
            "use" => Operation::Use,
            "install" => Operation::Install,
            "default" => Operation::Default,
            "uninstall" => {
                confirm_uninstall(&version.value)?;
                Operation::Uninstall
            }
            _ => anyhow::bail!("unsupported fnm action {action}"),
        };
        perform(&SystemRunner, &self.origin_pane, &version.value, operation)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Use,
    Install,
    Default,
    Uninstall,
}

fn perform(
    runner: &dyn CommandRunner,
    origin_pane: &str,
    version: &str,
    operation: Operation,
) -> Result<ActionOutcome> {
    anyhow::ensure!(
        valid_version(version),
        "fnm returned an unsafe version token"
    );
    let ok = match operation {
        Operation::Use => {
            anyhow::ensure!(
                !origin_pane.is_empty(),
                "Switchboard could not identify the pane that should use Node {version}"
            );
            let command = format!("fnm use {version}");
            runner.ok("herdr", &["pane", "run", origin_pane, &command])
        }
        Operation::Install => runner.ok("fnm", &["install", version]),
        Operation::Default => runner.ok("fnm", &["default", version]),
        Operation::Uninstall => runner.ok("fnm", &["uninstall", version]),
    };
    anyhow::ensure!(ok, "fnm could not {} {version}", operation.verb());
    Ok(if operation == Operation::Use {
        ActionOutcome::Close
    } else {
        ActionOutcome::StayOpen
    })
}

impl Operation {
    fn verb(self) -> &'static str {
        match self {
            Self::Use => "use",
            Self::Install => "install",
            Self::Default => "set the default to",
            Self::Uninstall => "uninstall",
        }
    }
}

fn load_installed(runner: &dyn CommandRunner) -> Result<Vec<Version>> {
    let output = runner
        .output("fnm", &["list"])
        .context("fnm is not installed or is not reachable from Switchboard")?;
    anyhow::ensure!(
        output.status.success(),
        "fnm could not list installed versions"
    );
    let current = runner.capture("fnm", &["current"]).unwrap_or_default();
    let default = runner.capture("fnm", &["default"]).unwrap_or_default();
    Ok(
        parse_versions(&String::from_utf8_lossy(&output.stdout), Source::Installed)
            .into_iter()
            .map(|mut version| {
                version.current = same_version(&version.value, &current);
                version.default = version.default || same_version(&version.value, &default);
                version
            })
            .collect(),
    )
}

fn load_remote(runner: &dyn CommandRunner) -> Result<Vec<Version>> {
    let output = runner
        .output("fnm", &["list-remote", "--sort", "desc"])
        .context("fnm could not start the remote version lookup")?;
    anyhow::ensure!(
        output.status.success(),
        "fnm could not list remote versions"
    );
    Ok(parse_versions(
        &String::from_utf8_lossy(&output.stdout),
        Source::Remote,
    ))
}

fn parse_versions(output: &str, source: Source) -> Vec<Version> {
    output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim().trim_start_matches('*').trim();
            let value = trimmed.split_whitespace().next()?.to_string();
            valid_version(&value).then(|| Version {
                default: trimmed.split_whitespace().any(|part| part == "default"),
                value,
                source,
                current: false,
            })
        })
        .collect()
}

fn valid_version(value: &str) -> bool {
    if value == "system" {
        return true;
    }
    let numeric = value.strip_prefix('v').unwrap_or(value);
    numeric.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && numeric
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn same_version(left: &str, right: &str) -> bool {
    left.trim_start_matches('v') == right.trim().trim_start_matches('v')
}

fn version_item(version: &Version) -> PickerItem {
    let source = match version.source {
        Source::Installed => "installed",
        Source::Remote => "remote",
    };
    let mut statuses = Vec::new();
    if version.current {
        statuses.push("current");
    }
    if version.default {
        statuses.push("default");
    }
    let status = statuses.join(",");
    let detail = if status.is_empty() {
        source.to_string()
    } else {
        format!("{source} · {}", statuses.join(" · "))
    };
    let action = if version.source == Source::Installed {
        "Enter uses this version in the pane that opened Switchboard."
    } else {
        "Enter installs this version with fnm."
    };
    PickerItem {
        id: version.id(),
        primary: version.value.clone(),
        secondary: detail,
        trailing: (!status.is_empty()).then_some(status.clone()),
        document: Document {
            fuzzy: format!("{} {source} {status}", version.value),
            fields: HashMap::from([
                ("version".into(), version.value.clone()),
                ("source".into(), source.into()),
                ("status".into(), status),
            ]),
        },
        preview: vec![
            "Node version".into(),
            String::new(),
            format!("version  {}", version.value),
            format!("source   {source}"),
            format!("current  {}", if version.current { "yes" } else { "no" }),
            format!("default  {}", if version.default { "yes" } else { "no" }),
            String::new(),
            action.into(),
        ],
        accent_slot: Some(if version.source == Source::Installed {
            "green".into()
        } else {
            "blue".into()
        }),
    }
}

fn status_item(id: &str, title: &str, detail: &str) -> PickerItem {
    PickerItem {
        id: format!("status:{id}"),
        primary: title.into(),
        secondary: detail.into(),
        trailing: None,
        document: Document {
            fuzzy: format!("{title} {detail}"),
            fields: HashMap::from([("source".into(), "status".into())]),
        },
        preview: vec![title.into(), String::new(), detail.into()],
        accent_slot: Some("yellow".into()),
    }
}

fn confirm_uninstall(version: &str) -> Result<()> {
    println!("\x1b[1mUninstall Node {version}?\x1b[0m\n");
    print!("Type {version} to confirm: ");
    io::stdout().flush()?;
    let mut reply = String::new();
    io::stdin().read_line(&mut reply)?;
    anyhow::ensure!(reply.trim() == version, "uninstall cancelled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    #[test]
    fn installed_output_marks_current_and_default_from_explicit_queries() {
        let runner = MockRunner::new()
            .on("fnm list", "* v18.20.8\n* v24.18.0 default\n* system\n")
            .on("fnm current", "v18.20.8\n")
            .on("fnm default", "v24.18.0\n");
        let versions = load_installed(&runner).unwrap();

        assert_eq!(versions.len(), 3);
        assert!(versions[0].current);
        assert!(versions[1].default);
        assert_eq!(versions[2].value, "system");
    }

    #[test]
    fn remote_output_ignores_messages_and_unsafe_tokens() {
        let versions = parse_versions(
            "v24.1.0\nv22.16.0\n../../bin/sh\nDownloading versions…\n",
            Source::Remote,
        );
        assert_eq!(
            versions
                .iter()
                .map(|version| version.value.as_str())
                .collect::<Vec<_>>(),
            vec!["v24.1.0", "v22.16.0"]
        );
    }

    #[test]
    fn use_targets_the_origin_pane_and_closes_the_manager() {
        let runner = MockRunner::new();
        let outcome = perform(&runner, "pane-7", "v24.18.0", Operation::Use).unwrap();
        assert!(matches!(outcome, ActionOutcome::Close));
        assert_eq!(
            runner.calls(),
            vec![vec!["herdr", "pane", "run", "pane-7", "fnm use v24.18.0"]]
        );
    }

    #[test]
    fn mutations_use_fnm_argv_and_keep_the_manager_open() {
        for (operation, verb) in [
            (Operation::Install, "install"),
            (Operation::Default, "default"),
            (Operation::Uninstall, "uninstall"),
        ] {
            let runner = MockRunner::new();
            let outcome = perform(&runner, "", "v22.16.0", operation).unwrap();
            assert!(matches!(outcome, ActionOutcome::StayOpen));
            assert_eq!(runner.calls()[0], vec!["fnm", verb, "v22.16.0"]);
        }
    }

    #[test]
    fn unsafe_version_never_reaches_a_command_line() {
        let runner = MockRunner::new();
        assert!(perform(&runner, "pane-7", "24; rm -rf x", Operation::Use).is_err());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn installed_versions_are_not_duplicated_in_the_remote_rows() {
        let mut mode = FnmMode::new();
        mode.installed = parse_versions("v24.1.0\n", Source::Installed);
        mode.remote = parse_versions("v25.0.0\nv24.1.0\n", Source::Remote);
        let ids = mode
            .items()
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["installed:v24.1.0", "remote:v25.0.0"]);
    }
}
