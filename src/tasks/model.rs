use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::require;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub goal: String,
    pub repo: String,
    #[serde(default = "head")]
    pub base: String,
    pub scope: String,
    pub acceptance: String,
    #[serde(default)]
    pub constraints: String,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub decisions: String,
    #[serde(default)]
    pub tried: String,
    #[serde(default)]
    pub open_questions: String,
    pub expected_result: String,
}

fn head() -> String {
    "HEAD".into()
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        require(
            [
                &self.goal,
                &self.scope,
                &self.acceptance,
                &self.expected_result,
            ]
            .iter()
            .all(|s| !s.trim().is_empty()),
            "invalid_request",
            "goal, scope, acceptance and expected_result are required",
        )?;
        require(
            std::path::Path::new(&self.repo).is_absolute(),
            "invalid_request",
            "repo must be an absolute path",
        )?;
        require(
            !self.base.is_empty() && !self.base.starts_with('-'),
            "invalid_request",
            "base must be a Git revision",
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Status {
    Provisioning,
    Unconfirmed,
    Submitted,
    Active,
    NeedsHelp,
    Released,
    Completed,
    Failed,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Unconfirmed => "unconfirmed",
            Self::Submitted => "submitted",
            Self::Active => "active",
            Self::NeedsHelp => "needs_help",
            Self::Released => "released",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn settled(self) -> bool {
        matches!(
            self,
            Self::NeedsHelp | Self::Released | Self::Completed | Self::Failed | Self::Unconfirmed
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    pub progress: String,
    pub decisions: String,
    pub tried: String,
    pub remaining: String,
    pub next_step: String,
    pub verification: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Report {
    pub status: Status,
    pub summary: String,
    pub tests: Vec<String>,
    pub artifacts: Vec<String>,
}

impl Report {
    pub fn validate(&self) -> Result<()> {
        require(
            matches!(
                self.status,
                Status::Completed | Status::Failed | Status::NeedsHelp
            ),
            "invalid_report",
            "report status must be completed, failed or needs_help",
        )?;
        require(
            !self.summary.trim().is_empty(),
            "invalid_report",
            "report summary is required",
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Target {
    pub pane: String,
    pub name: String,
    pub kind: String,
    pub native_session: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Evidence {
    pub head: String,
    pub branch: String,
    pub dirty: bool,
    pub changed_files: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Task {
    pub id: String,
    pub generation: u64,
    pub status: Status,
    pub request: Request,
    pub source_pane: String,
    pub base_commit: String,
    pub source_dirty: bool,
    pub branch: String,
    pub worktree: Option<String>,
    pub workspace: Option<String>,
    pub target: Option<Target>,
    pub server_stamp: String,
    pub checkpoints: Vec<Checkpoint>,
    pub report: Option<Report>,
    pub evidence: Option<Evidence>,
    pub step: String,
    pub error_code: Option<String>,
    pub updated_at: u64,
}

impl Task {
    pub fn generation_is(&self, generation: u64) -> Result<()> {
        require(
            self.generation == generation,
            "stale_generation",
            "task generation has changed",
        )
    }

    pub fn claim(&mut self, generation: u64) -> Result<()> {
        self.generation_is(generation)?;
        require(
            matches!(
                self.status,
                Status::Submitted | Status::Unconfirmed | Status::Active
            ),
            "invalid_transition",
            "task is not available to claim",
        )?;
        self.status = Status::Active;
        self.step = "await_report".into();
        self.error_code = None;
        Ok(())
    }

    pub fn report(&mut self, generation: u64, report: Report, evidence: Evidence) -> Result<()> {
        self.generation_is(generation)?;
        report.validate()?;
        if self.report.as_ref() == Some(&report) && self.status == report.status {
            return Ok(());
        }
        require(
            matches!(self.status, Status::Active | Status::NeedsHelp),
            "invalid_transition",
            "claim the task before reporting; terminal reports cannot be replaced",
        )?;
        self.status = report.status;
        self.step = "reported".into();
        self.report = Some(report);
        self.evidence = Some(evidence);
        Ok(())
    }

    pub fn release(&mut self, generation: u64, checkpoint: Checkpoint) -> Result<()> {
        self.generation_is(generation)?;
        require(
            matches!(self.status, Status::Active | Status::NeedsHelp),
            "invalid_transition",
            "only the task owner can release active work",
        )?;
        require(
            !checkpoint.next_step.trim().is_empty() && !checkpoint.progress.trim().is_empty(),
            "invalid_checkpoint",
            "progress and next_step are required",
        )?;
        self.checkpoints.push(checkpoint);
        self.status = Status::Released;
        self.step = "await_handoff".into();
        Ok(())
    }
}
