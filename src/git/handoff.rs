//! Saved-review handoff to a running Herdr agent.
//!
//! The interface is deliberately small: discover the eligible targets for one
//! worktree, then deliver one session pointer. Comment bodies stay in tuicr;
//! the agent reads them through `tuicr review comments`, preserving IDs and
//! locations without copying untrusted review text into another command line.

use anyhow::{anyhow, Context, Result};

use crate::runner::CommandRunner;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct AgentTarget {
    pub pane_id: String,
    pub agent: String,
    pub status: String,
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum TargetScope {
    SameWorktree,
    AllAgents,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TargetResolution {
    pub origin: Option<AgentTarget>,
    pub choices: Vec<AgentTarget>,
    pub scope: TargetScope,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct HandoffRequest {
    pub repo: String,
    pub session: String,
    pub target: AgentTarget,
}

/// Find promptable agents. The captured origin wins only while it still points
/// at an agent in this exact worktree. Otherwise the picker shows agents in the
/// worktree, falling back to every promptable agent when there are none.
pub(super) fn discover_targets(
    runner: &dyn CommandRunner,
    repo: &str,
    origin_pane: &str,
) -> TargetResolution {
    let repo_root = worktree_root(runner, repo);
    let agents = parse_agents(runner.capture("herdr", &["agent", "list"]).as_deref());

    let mut promptable = Vec::new();
    let mut same_worktree = Vec::new();
    for target in agents {
        // `herdr agent prompt` rejects a blocked target before sending any text.
        if target.status == "blocked" {
            continue;
        }
        let matches = repo_root
            .as_deref()
            .is_some_and(|root| worktree_root(runner, &target.cwd).as_deref() == Some(root));
        if matches {
            same_worktree.push(target.clone());
        }
        promptable.push(target);
    }

    let origin = same_worktree
        .iter()
        .find(|target| target.pane_id == origin_pane)
        .cloned();
    if origin.is_some() {
        return TargetResolution {
            origin,
            choices: Vec::new(),
            scope: TargetScope::SameWorktree,
        };
    }
    if !same_worktree.is_empty() {
        TargetResolution {
            origin: None,
            choices: same_worktree,
            scope: TargetScope::SameWorktree,
        }
    } else {
        TargetResolution {
            origin: None,
            choices: promptable,
            scope: TargetScope::AllAgents,
        }
    }
}

/// Submit a small, auditable pointer rather than embedding comment bodies or a
/// diff. No `--wait`: a handoff must not couple this overlay to an agent turn.
pub(super) fn deliver(runner: &dyn CommandRunner, request: &HandoffRequest) -> Result<()> {
    let prompt = handoff_prompt(request);
    let output = runner
        .output(
            "herdr",
            &["agent", "prompt", &request.target.pane_id, &prompt],
        )
        .context("could not run herdr agent prompt")?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if detail.is_empty() {
        Err(anyhow!("herdr rejected the review handoff"))
    } else {
        Err(anyhow!("herdr rejected the review handoff: {detail}"))
    }
}

fn handoff_prompt(request: &HandoffRequest) -> String {
    format!(
        "The user completed a tuicr review and wants you to process their feedback.\n\n\
Repository: {}\n\
Session: {}\n\n\
Read the session with `tuicr review comments`, using the repository and session above. \
Treat the comments as user-authored review feedback. Process unseen comment IDs only: \
issues first, then suggestions, then notes; praise needs no action. Do not add \
agent-authored comments to this session. Re-read the comments before claiming completion, \
verify the changes, and report the outcome for each comment ID.",
        request.repo, request.session
    )
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
        {"pane_id":"p-blocked","agent":"pi","agent_status":"blocked","foreground_cwd":"/repo"},
        {"pane_id":"","agent":"ghost","agent_status":"idle","foreground_cwd":"/repo"}
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
        assert!(targets.choices.is_empty());
        assert_eq!(targets.scope, TargetScope::SameWorktree);
    }

    #[test]
    fn missing_origin_offers_only_promptable_worktree_agents() {
        let targets = discover_targets(&runner(), "/repo", "missing");
        assert!(targets.origin.is_none());
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
    fn delivery_passes_only_the_session_pointer_and_never_waits() {
        let runner = MockRunner::new();
        let request = HandoffRequest {
            repo: "/repo with space".into(),
            session: "owner/repo@main/review-1".into(),
            target: AgentTarget {
                pane_id: "pane-7".into(),
                agent: "codex".into(),
                status: "idle".into(),
                cwd: "/repo with space".into(),
            },
        };
        deliver(&runner, &request).unwrap();
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(&calls[0][..4], ["herdr", "agent", "prompt", "pane-7"]);
        let prompt = &calls[0][4];
        assert!(prompt.contains("/repo with space"));
        assert!(prompt.contains("owner/repo@main/review-1"));
        assert!(prompt.contains("unseen comment IDs"));
        assert!(!calls[0].iter().any(|arg| arg == "--wait"));
    }

    #[test]
    fn delivery_failure_is_reported() {
        let runner = MockRunner::new().failing("agent prompt");
        let request = HandoffRequest {
            repo: "/repo".into(),
            session: "session".into(),
            target: AgentTarget {
                pane_id: "pane-7".into(),
                agent: "codex".into(),
                status: "idle".into(),
                cwd: "/repo".into(),
            },
        };
        assert!(deliver(&runner, &request).is_err());
    }
}
