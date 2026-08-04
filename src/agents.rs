//! Installed AI integrations picker and agent launcher.
//!
//! This is deliberately separate from the Projects picker's running-agent source:
//! Projects answers “what can I focus?”, while this mode answers “which supported
//! agent can I start?”. Herdr remains the authority for both integration status and
//! agent startup.

use std::collections::{HashMap, HashSet};
use std::env;

use anyhow::{anyhow, Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};
use serde_json::Value;

use crate::config::Config;
use crate::data::Theme;
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema};
use crate::runner::{CommandRunner, SystemRunner};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Integration {
    id: String,
    kind: String,
    title: String,
    status: String,
    path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Target {
    Pane,
    Tab,
    Workspace,
}

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    let normal = cfg.common.keymode == "normal";
    let mode = AgentsMode {
        origin_pane: env::var("SWITCHBOARD_ORIGIN_PANE_ID").unwrap_or_default(),
        origin_cwd: env::var("SWITCHBOARD_ORIGIN_CWD").unwrap_or_default(),
        bindings: cfg.keys.get("agents").cloned().unwrap_or_default(),
        integrations: Vec::new(),
    };
    picker::run(mode, theme, normal)
}

struct AgentsMode {
    origin_pane: String,
    origin_cwd: String,
    bindings: HashMap<String, String>,
    integrations: Vec<Integration>,
}

impl PickerMode for AgentsMode {
    fn title(&self) -> &str {
        "AI Integrations"
    }

    fn accent_slot(&self) -> &'static str {
        "mauve"
    }

    fn schema(&self) -> FieldSchema {
        FieldSchema::default()
    }

    fn actions(&self) -> Vec<ActionSpec> {
        vec![
            ActionSpec {
                id: "pane",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                key_label: "↵",
                label: "pane",
                color_slot: "green",
            },
            ActionSpec {
                id: "tab",
                key: KeyCode::Char('t'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^t",
                label: "tab",
                color_slot: "blue",
            },
            ActionSpec {
                id: "workspace",
                key: KeyCode::Char('w'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥w",
                label: "workspace",
                color_slot: "mauve",
            },
        ]
    }

    fn key_bindings(&self) -> HashMap<String, String> {
        self.bindings.clone()
    }

    fn reload_config(&mut self, cfg: &Config) -> Result<()> {
        self.bindings = cfg.keys.get("agents").cloned().unwrap_or_default();
        Ok(())
    }

    fn initial(&mut self) -> Result<Vec<PickerItem>> {
        self.integrations = load_integrations(&SystemRunner)?;
        Ok(self
            .integrations
            .iter()
            .map(|integration| PickerItem {
                id: integration.id.clone(),
                primary: integration.title.clone(),
                secondary: integration.status.clone(),
                trailing: Some(integration.kind.clone()),
                document: Document {
                    fuzzy: format!(
                        "{} {} {} {}",
                        integration.title, integration.id, integration.kind, integration.status
                    ),
                    fields: Default::default(),
                },
                preview: vec![
                    "Installed AI integration".into(),
                    String::new(),
                    integration.title.clone(),
                    format!("kind    {}", integration.kind),
                    format!("status  {}", integration.status),
                    format!("hook    {}", integration.path),
                    String::new(),
                    "Enter starts it in the origin pane.".into(),
                    "Ctrl-T creates a tab; Alt-W creates a workspace.".into(),
                ],
                accent_slot: Some("mauve".into()),
            })
            .collect())
    }

    fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome> {
        let integration = self
            .integrations
            .iter()
            .find(|integration| integration.id == item_id)
            .ok_or_else(|| anyhow!("integration {item_id} is no longer available"))?;
        let target = match action {
            "pane" => Target::Pane,
            "tab" => Target::Tab,
            "workspace" => Target::Workspace,
            _ => return Err(anyhow!("unknown agent target {action}")),
        };
        launch(
            &SystemRunner,
            integration,
            target,
            &self.origin_pane,
            &self.origin_cwd,
        )?;
        Ok(ActionOutcome::Close)
    }
}

fn load_integrations(runner: &dyn CommandRunner) -> Result<Vec<Integration>> {
    let output = runner
        .capture("herdr", &["integration", "status"])
        .ok_or_else(|| anyhow!("herdr integration status failed"))?;
    Ok(parse_integrations(&output))
}

fn parse_integrations(output: &str) -> Vec<Integration> {
    output
        .lines()
        .filter_map(|line| {
            let (id, detail) = line.split_once(": ")?;
            if detail.starts_with("not installed") {
                return None;
            }
            let (status, path) = detail
                .rfind(" (")
                .filter(|_| detail.ends_with(')'))
                .map(|start| {
                    (
                        detail[..start].to_string(),
                        detail[start + 2..detail.len() - 1].to_string(),
                    )
                })
                .unwrap_or_else(|| (detail.to_string(), String::new()));
            Some(Integration {
                id: id.to_string(),
                kind: agent_kind(id).to_string(),
                title: display_name(id),
                status,
                path,
            })
        })
        .collect()
}

fn agent_kind(integration: &str) -> &str {
    match integration {
        // Herdr calls the integration by its product name and the start kind by
        // its canonical detector name.
        "antigravity-cli" => "agy",
        other => other,
    }
}

fn display_name(id: &str) -> String {
    match id {
        "pi" => "Pi".into(),
        "omp" => "Oh My Pi".into(),
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "copilot" => "GitHub Copilot".into(),
        "devin" => "Devin".into(),
        "droid" => "Droid".into(),
        "kimi" => "Kimi".into(),
        "opencode" => "OpenCode".into(),
        "kilo" => "Kilo Code".into(),
        "hermes" => "Hermes".into(),
        "qodercli" => "Qoder CLI".into(),
        "cursor" => "Cursor".into(),
        "mastracode" => "Mastra Code".into(),
        "antigravity-cli" => "Antigravity".into(),
        "grok" => "Grok".into(),
        other => other.to_string(),
    }
}

#[derive(Debug)]
struct CreatedTarget {
    pane_id: String,
    rollback_kind: Option<&'static str>,
    rollback_id: Option<String>,
}

fn launch(
    runner: &dyn CommandRunner,
    integration: &Integration,
    target: Target,
    origin_pane: &str,
    origin_cwd: &str,
) -> Result<()> {
    let created = prepare_target(runner, target, origin_pane, origin_cwd, &integration.title)?;
    let name = next_agent_name(runner, &integration.kind);
    let started = runner.ok(
        "herdr",
        &[
            "agent",
            "start",
            &name,
            "--kind",
            &integration.kind,
            "--pane",
            &created.pane_id,
        ],
    );
    if started {
        return Ok(());
    }

    // A tab/workspace was created solely for this launch. Do not leave an empty
    // focused target behind when Herdr rejects or times out starting the agent.
    if let (Some(kind), Some(id)) = (created.rollback_kind, created.rollback_id.as_deref()) {
        let _ = runner.ok("herdr", &[kind, "close", id]);
    }
    Err(anyhow!("could not start {}", integration.title))
}

/// Pick the canonical kind when it is free, then a stable numbered suffix. Herdr
/// requires live agent names to be unique; using the integration id directly would
/// make starting a second Claude/Codex session fail before the process launched.
fn next_agent_name(runner: &dyn CommandRunner, kind: &str) -> String {
    let names: HashSet<String> = runner
        .capture("herdr", &["agent", "list"])
        .and_then(|output| serde_json::from_str::<Value>(&output).ok())
        .and_then(|value| value["result"]["agents"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|agent| agent["agent"].as_str().map(str::to_string))
        .collect();
    if !names.contains(kind) {
        return kind.to_string();
    }
    (2..10_000)
        .map(|suffix| format!("{kind}-{suffix}"))
        .find(|candidate| !names.contains(candidate))
        .unwrap_or_else(|| format!("{kind}-{}", std::process::id()))
}

fn prepare_target(
    runner: &dyn CommandRunner,
    target: Target,
    origin_pane: &str,
    origin_cwd: &str,
    label: &str,
) -> Result<CreatedTarget> {
    match target {
        Target::Pane => {
            if origin_pane.is_empty() {
                return Err(anyhow!("no origin pane to start the agent in"));
            }
            Ok(CreatedTarget {
                pane_id: origin_pane.to_string(),
                rollback_kind: None,
                rollback_id: None,
            })
        }
        Target::Tab => {
            let workspace = origin_workspace(runner, origin_pane)?;
            let args = create_args("tab", Some(&workspace), origin_cwd, label);
            let value = capture_json(runner, "herdr", &args)?;
            created_from_json(&value, "tab", "tab_id")
        }
        Target::Workspace => {
            let args = create_args("workspace", None, origin_cwd, label);
            let value = capture_json(runner, "herdr", &args)?;
            created_from_json(&value, "workspace", "workspace_id")
        }
    }
}

fn create_args<'a>(
    kind: &'a str,
    workspace: Option<&'a str>,
    cwd: &'a str,
    label: &'a str,
) -> Vec<&'a str> {
    let mut args = vec![kind, "create"];
    if let Some(workspace) = workspace {
        args.extend_from_slice(&["--workspace", workspace]);
    }
    if !cwd.is_empty() {
        args.extend_from_slice(&["--cwd", cwd]);
    }
    args.extend_from_slice(&["--label", label, "--focus"]);
    args
}

fn origin_workspace(runner: &dyn CommandRunner, origin_pane: &str) -> Result<String> {
    if origin_pane.is_empty() {
        return Err(anyhow!("no origin pane for the new tab"));
    }
    let value = capture_json(runner, "herdr", &["pane", "get", origin_pane])?;
    value["result"]["pane"]["workspace_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("origin pane has no workspace"))
}

fn capture_json(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> Result<Value> {
    let output = runner
        .capture(program, args)
        .ok_or_else(|| anyhow!("{} failed", args.join(" ")))?;
    serde_json::from_str(&output).with_context(|| format!("invalid {} response", args.join(" ")))
}

fn created_from_json(value: &Value, kind: &'static str, id_key: &str) -> Result<CreatedTarget> {
    let root = &value["result"]["root_pane"];
    let pane_id = root["pane_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("{kind} create returned no root pane"))?;
    let rollback_id = match kind {
        "tab" => value["result"]["tab"][id_key].as_str(),
        "workspace" => value["result"]["workspace"][id_key].as_str(),
        _ => None,
    }
    .filter(|id| !id.is_empty())
    .ok_or_else(|| anyhow!("{kind} create returned no target id"))?;
    Ok(CreatedTarget {
        pane_id: pane_id.to_string(),
        rollback_kind: Some(kind),
        rollback_id: Some(rollback_id.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    const STATUS: &str = "pi: not installed (/home/u/.pi/hook)\nclaude: current (v7) (/home/u/.claude/hook)\nantigravity-cli: outdated (v1 -> v2) (/home/u/.gemini/hook)\n";

    fn integration() -> Integration {
        Integration {
            id: "claude".into(),
            kind: "claude".into(),
            title: "Claude".into(),
            status: "current (v7)".into(),
            path: "/home/u/.claude/hook".into(),
        }
    }

    #[test]
    fn status_parser_keeps_only_installed_integrations() {
        let integrations = parse_integrations(STATUS);
        assert_eq!(integrations.len(), 2);
        assert_eq!(integrations[0], integration());
        assert_eq!(integrations[1].kind, "agy");
        assert_eq!(integrations[1].status, "outdated (v1 -> v2)");
    }

    #[test]
    fn pane_launch_starts_the_selected_kind_in_the_origin() {
        let runner = MockRunner::new();
        launch(&runner, &integration(), Target::Pane, "w1:p1", "/repo").unwrap();
        assert_eq!(
            runner.calls(),
            vec![
                vec!["herdr", "agent", "list"],
                vec!["herdr", "agent", "start", "claude", "--kind", "claude", "--pane", "w1:p1"]
            ]
        );
    }

    #[test]
    fn launch_numbers_a_name_that_is_already_live() {
        let runner = MockRunner::new().on(
            "agent list",
            r#"{"result":{"agents":[{"agent":"claude"},{"agent":"claude-2"}]}}"#,
        );
        launch(&runner, &integration(), Target::Pane, "w1:p1", "/repo").unwrap();
        assert!(runner.calls()[1].contains(&"claude-3".into()));
    }

    #[test]
    fn tab_launch_resolves_the_origin_workspace_and_starts_in_the_root_pane() {
        let runner = MockRunner::new()
            .on(
                "pane get w1:p1",
                r#"{"result":{"pane":{"workspace_id":"w1"}}}"#,
            )
            .on(
                "tab create",
                r#"{"result":{"root_pane":{"pane_id":"w1:p2"},"tab":{"tab_id":"w1:t2"}}}"#,
            );
        launch(&runner, &integration(), Target::Tab, "w1:p1", "/repo").unwrap();
        let calls = runner.calls();
        assert_eq!(calls[0], vec!["herdr", "pane", "get", "w1:p1"]);
        assert_eq!(
            calls[1],
            vec![
                "herdr",
                "tab",
                "create",
                "--workspace",
                "w1",
                "--cwd",
                "/repo",
                "--label",
                "Claude",
                "--focus"
            ]
        );
        assert_eq!(calls[2], vec!["herdr", "agent", "list"]);
        assert!(calls[3].ends_with(&["--pane".into(), "w1:p2".into()]));
    }

    #[test]
    fn failed_workspace_start_closes_the_workspace_it_created() {
        let runner = MockRunner::new()
            .on(
                "workspace create",
                r#"{"result":{"root_pane":{"pane_id":"w2:p1"},"workspace":{"workspace_id":"w2"}}}"#,
            )
            .failing("agent start");
        let error =
            launch(&runner, &integration(), Target::Workspace, "w1:p1", "/repo").unwrap_err();
        assert!(error.to_string().contains("could not start Claude"));
        assert_eq!(
            runner.calls().last().unwrap(),
            &vec!["herdr", "workspace", "close", "w2"]
        );
    }
}
