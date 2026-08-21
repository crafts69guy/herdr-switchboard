//! Saved-review-specific handoff prompt.
//!
//! Target discovery and prompt submission belong to the shared
//! [`crate::agent_handoff`] module. This module keeps only tuicr's pointer
//! vocabulary: comment bodies stay in tuicr and the agent reads them by session.

use anyhow::Result;

use crate::agent_handoff::{deliver_prompt, AgentTarget};
use crate::runner::CommandRunner;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct HandoffRequest {
    pub repo: String,
    pub session: String,
    pub target: AgentTarget,
}

pub(super) fn deliver(runner: &dyn CommandRunner, request: &HandoffRequest) -> Result<()> {
    deliver_prompt(runner, &request.target, &handoff_prompt(request))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    fn request() -> HandoffRequest {
        HandoffRequest {
            repo: "/repo with space".into(),
            session: "owner/repo@main/review-1".into(),
            target: AgentTarget {
                pane_id: "pane-7".into(),
                agent: "codex".into(),
                status: "idle".into(),
                cwd: "/repo with space".into(),
            },
        }
    }

    #[test]
    fn delivery_passes_only_the_session_pointer_and_never_waits() {
        let runner = MockRunner::new();
        deliver(&runner, &request()).unwrap();
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
        assert!(deliver(&runner, &request()).is_err());
    }
}
