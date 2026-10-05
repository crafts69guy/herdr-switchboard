//! Shared target discovery and prompt delivery for handoffs to running Herdr agents.

use anyhow::{anyhow, Context, Result};

use crate::runner::CommandRunner;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentTarget {
    pub pane_id: String,
    pub agent: String,
    pub status: String,
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TargetScope {
    SameWorktree,
    SameDirectory,
    AllAgents,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TargetResolution {
    pub origin: Option<AgentTarget>,
    pub choices: Vec<AgentTarget>,
    pub scope: TargetScope,
}

/// Find promptable agents for `path`. The captured origin wins only when it is
/// still in the same worktree (or exact non-Git directory); otherwise callers
/// receive matching choices, falling back to every promptable running agent.
pub(crate) fn discover_targets(
    runner: &dyn CommandRunner,
    path: &str,
    origin_pane: &str,
) -> TargetResolution {
    let path_root = worktree_root(runner, path);
    let matching_scope = if path_root.is_some() {
        TargetScope::SameWorktree
    } else {
        TargetScope::SameDirectory
    };
    let agents = parse_agents(runner.capture("herdr", &["agent", "list"]).as_deref());

    let mut promptable = Vec::new();
    let mut matching = Vec::new();
    for target in agents {
        if target.status == "blocked" {
            continue;
        }
        let matches = match path_root.as_deref() {
            Some(root) => worktree_root(runner, &target.cwd).as_deref() == Some(root),
            None => same_directory(path, &target.cwd),
        };
        if matches {
            matching.push(target.clone());
        }
        promptable.push(target);
    }

    let origin = matching
        .iter()
        .find(|target| target.pane_id == origin_pane)
        .cloned();
    if origin.is_some() {
        return TargetResolution {
            origin,
            choices: matching,
            scope: matching_scope,
        };
    }
    if matching.is_empty() {
        TargetResolution {
            origin: None,
            choices: promptable,
            scope: TargetScope::AllAgents,
        }
    } else {
        TargetResolution {
            origin: None,
            choices: matching,
            scope: matching_scope,
        }
    }
}

/// Submit one prompt without `--wait`; a handoff must not couple the source
/// surface to the receiving agent's turn.
pub(crate) fn deliver_prompt(
    runner: &dyn CommandRunner,
    target: &AgentTarget,
    prompt: &str,
) -> Result<()> {
    let output = runner
        .output("herdr", &["agent", "prompt", &target.pane_id, prompt])
        .context("could not run herdr agent prompt")?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if detail.is_empty() {
        Err(anyhow!("herdr rejected the agent handoff"))
    } else {
        Err(anyhow!("herdr rejected the agent handoff: {detail}"))
    }
}

fn same_directory(left: &str, right: &str) -> bool {
    !left.is_empty() && left.trim_end_matches('/') == right.trim_end_matches('/')
}

fn parse_agents(json: Option<&str>) -> Vec<AgentTarget> {
    let Some(value) = json.and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
    else {
        return Vec::new();
    };
    let Some(agents) = value["result"]["agents"].as_array() else {
        return Vec::new();
    };
    agents
        .iter()
        .filter_map(|agent| {
            let pane_id = agent["pane_id"].as_str()?.trim();
            let name = agent["agent"].as_str()?.trim();
            if pane_id.is_empty() || name.is_empty() {
                return None;
            }
            Some(AgentTarget {
                pane_id: pane_id.to_string(),
                agent: name.to_string(),
                status: agent["agent_status"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                cwd: agent["foreground_cwd"]
                    .as_str()
                    .or_else(|| agent["cwd"].as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

fn worktree_root(runner: &dyn CommandRunner, cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    runner
        .capture("git", &["-C", cwd, "rev-parse", "--show-toplevel"])
        .filter(|root| !root.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    const AGENTS: &str = r#"{"result":{"agents":[
        {"pane_id":"p-origin","agent":"codex","agent_status":"idle","foreground_cwd":"/repo/src"},
        {"pane_id":"p-peer","agent":"claude","agent_status":"working","foreground_cwd":"/repo/tests"},
        {"pane_id":"p-other","agent":"gemini","agent_status":"idle","foreground_cwd":"/other"},
        {"pane_id":"p-blocked","agent":"pi","agent_status":"blocked","foreground_cwd":"/repo"}
    ]}}"#;

    fn runner() -> MockRunner {
        MockRunner::new()
            .on("herdr agent list", AGENTS)
            .on("git -C /repo rev-parse", "/repo")
            .on("git -C /repo/src rev-parse", "/repo")
            .on("git -C /repo/tests rev-parse", "/repo")
            .on("git -C /other rev-parse", "/other")
    }

    #[test]
    fn exact_origin_in_the_worktree_wins() {
        let targets = discover_targets(&runner(), "/repo", "p-origin");
        assert_eq!(targets.origin.unwrap().agent, "codex");
        assert_eq!(targets.choices.len(), 2);
        assert_eq!(targets.scope, TargetScope::SameWorktree);
    }

    #[test]
    fn missing_origin_offers_only_promptable_worktree_agents() {
        let targets = discover_targets(&runner(), "/repo", "missing");
        assert_eq!(
            targets
                .choices
                .iter()
                .map(|target| target.pane_id.as_str())
                .collect::<Vec<_>>(),
            ["p-origin", "p-peer"]
        );
    }

    #[test]
    fn no_worktree_match_falls_back_to_all_promptable_agents() {
        let targets = discover_targets(&runner(), "/nowhere", "missing");
        assert_eq!(targets.scope, TargetScope::AllAgents);
        assert_eq!(targets.choices.len(), 3);
        assert!(targets
            .choices
            .iter()
            .all(|target| target.status != "blocked"));
    }

    #[test]
    fn non_git_paths_match_exact_current_directory() {
        let runner = MockRunner::new().on("herdr agent list", AGENTS);
        let targets = discover_targets(&runner, "/repo/src/", "missing");
        assert_eq!(targets.scope, TargetScope::SameDirectory);
        assert_eq!(targets.choices[0].pane_id, "p-origin");
    }

    #[test]
    fn delivery_never_waits_for_the_agent_turn() {
        let runner = MockRunner::new();
        let target = AgentTarget {
            pane_id: "pane-7".into(),
            agent: "codex".into(),
            status: "idle".into(),
            cwd: "/repo".into(),
        };
        deliver_prompt(&runner, &target, "context").unwrap();
        let calls = runner.calls();
        assert_eq!(
            &calls[0],
            &["herdr", "agent", "prompt", "pane-7", "context"]
        );
        assert!(!calls[0].iter().any(|arg| arg == "--wait"));
    }

    /// herdr's refusal is passed on with what it said, malformed agent lists
    /// read as no agents, and an agent with no pane or name is skipped.
    #[test]
    fn refusals_carry_herdrs_reason_and_malformed_lists_read_as_empty() {
        let target = AgentTarget {
            pane_id: "w1:p1".into(),
            agent: "claude".into(),
            status: "idle".into(),
            cwd: "/repo".into(),
        };
        let blocked = MockRunner::new().failing_with("agent prompt", "agent is blocked");
        let error = deliver_prompt(&blocked, &target, "hi").unwrap_err();
        assert!(error.to_string().ends_with("agent is blocked"), "{error}");
        let silent = MockRunner::new().failing("agent prompt");
        let error = deliver_prompt(&silent, &target, "hi").unwrap_err();
        assert!(error.to_string().ends_with("handoff"), "{error}");

        assert!(parse_agents(Some(r#"{"result":{}}"#)).is_empty());
        assert!(parse_agents(Some(
            r#"{"result":{"agents":[{"pane_id":" ","agent":"claude"}]}}"#
        ))
        .is_empty());
        assert_eq!(worktree_root(&MockRunner::new(), ""), None);
    }
}
