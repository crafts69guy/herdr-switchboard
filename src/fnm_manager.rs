//! Interactive fnm version manager.
//!
//! Local versions are loaded before the first frame. Remote versions cross a
//! background effect seam because `fnm list-remote` performs network work.
//! Mutations run only after the picker has restored the terminal.

use std::collections::HashSet;
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

    /// Only while a `fnm list-remote` lookup is actually outstanding — the same
    /// receiver `poll` reads, so the two cannot disagree.
    fn is_polling(&self) -> bool {
        self.remote_rx.is_some()
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
        trailing_marker: None,
        document: Document::new(
            format!("{} {source} {status}", version.value),
            &[
                ("version", version.value.clone()),
                ("source", source.into()),
                ("status", status),
            ],
        ),
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
        trailing_marker: None,
        document: Document::new(format!("{title} {detail}"), &[("source", "status".into())]),
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

    fn version(value: &str, source: Source, current: bool, default: bool) -> Version {
        Version {
            value: value.into(),
            source,
            current,
            default,
        }
    }

    /// Every action is gated on where the version came from, and getting one
    /// wrong offers an operation fnm will simply refuse — or worse, offers
    /// `uninstall` on the system Node.
    #[test]
    fn each_action_is_disabled_for_the_versions_it_cannot_apply_to() {
        let mut mode = FnmMode::new();
        mode.installed = vec![
            version("v24.1.0", Source::Installed, true, false),
            version("system", Source::Installed, false, false),
        ];
        mode.remote = vec![version("v25.0.0", Source::Remote, false, false)];

        let installed = "installed:v24.1.0";
        let remote = "remote:v25.0.0";

        // Installed: can be used, defaulted and removed; cannot be installed.
        assert!(mode.action_disabled_reason(installed, "use").is_none());
        assert!(mode.action_disabled_reason(installed, "default").is_none());
        assert!(mode
            .action_disabled_reason(installed, "uninstall")
            .is_none());
        let already = mode
            .action_disabled_reason(installed, "install")
            .expect("an installed version cannot be installed");
        assert!(already.contains("already installed"), "{already}");

        // Remote: can only be installed.
        for action in ["use", "default"] {
            let reason = mode
                .action_disabled_reason(remote, action)
                .unwrap_or_else(|| panic!("{action} needs an installed version"));
            assert!(reason.contains("install this version first"), "{reason}");
        }
        let removal = mode
            .action_disabled_reason(remote, "uninstall")
            .expect("a remote version is not installed");
        assert!(removal.contains("only installed"), "{removal}");

        // The system Node is not fnm's to remove.
        let system = mode
            .action_disabled_reason("installed:system", "uninstall")
            .expect("system Node must not be removable");
        assert!(system.contains("not managed by fnm"), "{system}");

        // Enter always offers *something*, and refresh never depends on a row.
        assert!(mode.action_disabled_reason(installed, "primary").is_none());
        assert!(mode.action_disabled_reason(remote, "primary").is_none());
        assert!(mode
            .action_disabled_reason("status:remote-loading", "refresh")
            .is_none());
    }

    /// A status row is not a version. Acting on one must be refused rather than
    /// resolved to whatever version happens to be nearby.
    #[test]
    fn a_status_row_is_not_actionable() {
        let mode = FnmMode::new();
        let reason = mode
            .action_disabled_reason("status:remote-loading", "primary")
            .expect("a status row offers no operation");
        assert!(reason.contains("refresh"), "{reason}");
    }

    /// The row has to say which version is live and which is default, because
    /// that is the entire question the manager exists to answer.
    #[test]
    fn a_version_row_states_its_source_and_both_status_flags() {
        let both = version_item(&version("v24.1.0", Source::Installed, true, true));
        assert_eq!(both.primary, "v24.1.0");
        assert_eq!(both.secondary, "installed · current · default");
        assert_eq!(both.trailing.as_deref(), Some("current,default"));
        assert_eq!(both.accent_slot.as_deref(), Some("green"));
        let card = both.preview.join("\n");
        assert!(card.contains("current  yes"), "{card}");
        assert!(card.contains("default  yes"), "{card}");
        assert!(card.contains("Enter uses this version"), "{card}");

        // A plain remote version carries no status tag at all.
        let remote = version_item(&version("v25.0.0", Source::Remote, false, false));
        assert_eq!(remote.secondary, "remote");
        assert_eq!(remote.trailing, None);
        assert_eq!(remote.accent_slot.as_deref(), Some("blue"));
        let card = remote.preview.join("\n");
        assert!(card.contains("current  no"), "{card}");
        assert!(card.contains("Enter installs this version"), "{card}");
    }

    /// Every documented filter field has to resolve against a real row.
    #[test]
    fn every_advertised_filter_field_matches_the_row_it_describes() {
        let mode = FnmMode::new();
        let schema = mode.schema();
        let item = version_item(&version("v24.1.0", Source::Installed, true, false));
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);

        for query in [
            "version:24.1",
            "v:24.1",
            "source:installed",
            "status:current",
        ] {
            let compiled = crate::query::CompiledQuery::compile(query, &schema)
                .unwrap_or_else(|error| panic!("`{query}` did not compile: {error:?}"));
            assert!(
                compiled.score(&item.document, &mut matcher).is_some(),
                "`{query}` matched no version"
            );
        }
    }

    /// While the remote lookup is outstanding the list says so, and when it
    /// fails it says that instead — silence would read as "there are none".
    #[test]
    fn the_remote_lookup_reports_itself_while_running_and_after_it_fails() {
        let (sender, receiver) = mpsc::channel();
        let mut mode = FnmMode::new();
        mode.installed = vec![version("v24.1.0", Source::Installed, false, false)];
        mode.remote_rx = Some(receiver);

        assert!(mode.is_polling());
        let loading = mode.items();
        assert_eq!(loading.last().unwrap().id, "status:remote-loading");
        assert!(mode.poll().is_none(), "nothing has been sent yet");

        sender
            .send(RemoteResult::Failed("network is down".into()))
            .unwrap();
        let items = mode.poll().expect("a result arrived").unwrap();
        assert!(!mode.is_polling(), "the lookup is finished");
        let status = items.last().unwrap();
        assert_eq!(status.id, "status:remote-error");
        assert_eq!(status.secondary, "network is down");
    }

    /// A worker that dies without answering must degrade to a stated failure,
    /// not to a list that claims it is still loading forever.
    #[test]
    fn a_remote_worker_that_disappears_becomes_a_stated_failure() {
        let (sender, receiver) = mpsc::channel::<RemoteResult>();
        let mut mode = FnmMode::new();
        mode.remote_rx = Some(receiver);
        drop(sender);

        let items = mode.poll().expect("a disconnect is an answer").unwrap();
        assert!(!mode.is_polling());
        assert_eq!(items.last().unwrap().id, "status:remote-error");
        assert!(
            mode.remote_error
                .as_deref()
                .unwrap()
                .contains("stopped unexpectedly"),
            "{:?}",
            mode.remote_error
        );
    }

    /// A successful lookup replaces the loading row with real versions.
    #[test]
    fn a_successful_remote_lookup_replaces_the_loading_row_with_versions() {
        let (sender, receiver) = mpsc::channel();
        let mut mode = FnmMode::new();
        mode.remote_rx = Some(receiver);
        sender
            .send(RemoteResult::Loaded(parse_versions(
                "v25.0.0\nv24.9.0\n",
                Source::Remote,
            )))
            .unwrap();

        let items = mode.poll().expect("a result arrived").unwrap();
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["remote:v25.0.0", "remote:v24.9.0"]);
    }

    /// Refresh is the one action that acts on the mode rather than a version,
    /// so it must keep the picker open and touch nothing.
    #[test]
    fn refresh_keeps_the_manager_open_without_running_anything() {
        let mut mode = FnmMode::new();
        assert_eq!(
            mode.execute("anything", "refresh").unwrap(),
            ActionOutcome::StayOpen
        );
    }

    /// Acting on a version that is no longer listed must fail by name rather
    /// than fall through to whichever version sorts first.
    #[test]
    fn acting_on_a_missing_version_fails_before_running_fnm() {
        let mut mode = FnmMode::new();
        let error = mode.execute("installed:v99.0.0", "use").unwrap_err();
        assert!(error.to_string().contains("no longer available"), "{error}");

        mode.installed = vec![version("v24.1.0", Source::Installed, false, false)];
        let error = mode.execute("installed:v24.1.0", "teleport").unwrap_err();
        assert!(
            error.to_string().contains("unsupported fnm action"),
            "{error}"
        );
    }

    /// The chrome the shared picker renders comes from these.
    #[test]
    fn the_mode_declares_its_title_accent_and_every_action() {
        let mode = FnmMode::new();
        assert_eq!(mode.title(), "Node Versions");
        assert_eq!(mode.accent_slot(), "green");
        assert_eq!(mode.list_pct(), 52);
        let ids: Vec<&str> = mode.actions().iter().map(|action| action.id).collect();
        assert_eq!(
            ids,
            [
                "primary",
                "use",
                "install",
                "default",
                "uninstall",
                "refresh"
            ]
        );
        assert!(mode.actions().iter().all(|a| !a.key_label.is_empty()));
    }

    /// `verb` is the phrase a failure is reported with, not the fnm subcommand,
    /// so it has to read as a sentence: "fnm could not <verb> v24.1.0".
    #[test]
    fn every_operation_reads_as_a_sentence_in_its_failure_message() {
        for (operation, expected) in [
            (Operation::Use, "fnm could not use v24.1.0"),
            (Operation::Install, "fnm could not install v24.1.0"),
            (
                Operation::Default,
                "fnm could not set the default to v24.1.0",
            ),
            (Operation::Uninstall, "fnm could not uninstall v24.1.0"),
        ] {
            let runner = MockRunner::new().failing("fnm");
            let error = perform(&runner, "w1:p1", "v24.1.0", operation)
                .expect_err("a failing fnm must be reported");
            assert_eq!(error.to_string(), expected);
        }
    }
}
