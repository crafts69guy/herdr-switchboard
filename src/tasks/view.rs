use std::collections::HashMap;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};

use super::effect::{Engine, Session};
use super::model::Task;
use super::require;
use crate::config::Config;
use crate::data::Theme;
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema, MatchKind};
use crate::runner::{CommandRunner, SystemRunner};

pub(crate) fn main(cfg: Config, theme: Theme) -> Result<()> {
    let mode = TasksMode::new(Session::load()?, Arc::new(SystemRunner), &cfg);
    picker::run(mode, theme, cfg)
}

struct TasksMode {
    session: Session,
    runner: Arc<dyn CommandRunner + Send + Sync>,
    bindings: HashMap<String, String>,
    items: Vec<PickerItem>,
    pending: Option<mpsc::Receiver<Result<Vec<PickerItem>>>>,
    next_refresh: Instant,
}

impl TasksMode {
    fn new(session: Session, runner: Arc<dyn CommandRunner + Send + Sync>, cfg: &Config) -> Self {
        Self {
            session,
            runner,
            bindings: cfg.keys.get("tasks").cloned().unwrap_or_default(),
            items: Vec::new(),
            pending: None,
            next_refresh: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let session = self.session.clone();
        let runner = Arc::clone(&self.runner);
        std::thread::spawn(move || {
            let result = snapshot(&Engine {
                session: &session,
                runner: runner.as_ref(),
            });
            let _ = tx.send(result);
        });
        self.pending = Some(rx);
    }
}

impl PickerMode for TasksMode {
    fn title(&self) -> &str {
        "Tasks"
    }
    fn accent_slot(&self) -> &'static str {
        "mauve"
    }
    fn schema(&self) -> FieldSchema {
        FieldSchema::new(
            &[
                ("status", MatchKind::Exact),
                ("agent", MatchKind::Exact),
                ("repo", MatchKind::Contains),
            ],
            &[],
        )
    }
    fn key_bindings(&self) -> HashMap<String, String> {
        self.bindings.clone()
    }
    fn reload_config(&mut self, cfg: &Config) -> Result<()> {
        self.bindings = cfg.keys.get("tasks").cloned().unwrap_or_default();
        Ok(())
    }
    fn empty_message(&self) -> &str {
        "No tasks yet. Agents can create one with task delegate."
    }
    fn actions(&self) -> Vec<ActionSpec> {
        vec![
            ActionSpec {
                id: "focus",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                key_label: "↵".into(),
                label: "agent",
                color_slot: "mauve",
            },
            ActionSpec {
                id: "task_worktree",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::CONTROL,
                key_label: "^↵".into(),
                label: "worktree",
                color_slot: "green",
            },
        ]
    }
    fn initial(&mut self) -> Result<Vec<PickerItem>> {
        self.refresh();
        Ok(self.items.clone())
    }
    fn is_polling(&self) -> bool {
        self.pending.is_some()
    }
    fn poll(&mut self) -> Option<Result<Vec<PickerItem>>> {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    self.pending = None;
                    self.next_refresh = Instant::now() + Duration::from_secs(2);
                    if let Ok(items) = &result {
                        self.items = items.clone();
                    }
                    return Some(result);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.next_refresh = Instant::now() + Duration::from_secs(2);
                    return Some(Err(anyhow::anyhow!(
                        "Task refresh stopped; retrying shortly"
                    )));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        } else if Instant::now() >= self.next_refresh {
            self.refresh();
        }
        None
    }
    fn execute(&mut self, id: &str, action: &str) -> Result<ActionOutcome> {
        let task = self.session.store.read()?.task(id)?.clone();
        let engine = Engine {
            session: &self.session,
            runner: self.runner.as_ref(),
        };
        match action {
            "focus" => {
                engine.validate_target(&task, false)?;
                engine.json(&[
                    "agent",
                    "focus",
                    &task.target.as_ref().context("missing target")?.name,
                ])?;
            }
            "task_worktree" => {
                require(
                    task.server_stamp == self.session.stamp,
                    "server_changed",
                    "task workspace must be rechecked after server restart",
                )?;
                let id = engine.validate_workspace(&task)?;
                engine.json(&["workspace", "focus", &id])?;
            }
            _ => anyhow::bail!("unknown task action"),
        }
        Ok(ActionOutcome::Close)
    }
}

fn snapshot(engine: &Engine<'_>) -> Result<Vec<PickerItem>> {
    let mut tasks = engine.session.store.read()?.tasks;
    tasks.reverse();
    let agents = if tasks.is_empty() {
        None
    } else {
        engine.json(&["agent", "list"]).ok()
    };
    Ok(tasks
        .iter()
        .map(|task| {
            let status = if task.server_stamp != engine.session.stamp {
                "server changed"
            } else {
                agents
                    .as_ref()
                    .and_then(|v| v["agents"].as_array())
                    .and_then(|agents| {
                        let target = task.target.as_ref()?;
                        agents.iter().find(|a| {
                            a["pane_id"].as_str() == Some(&target.pane)
                                && a["name"].as_str() == Some(&target.name)
                                && target
                                    .native_session
                                    .as_ref()
                                    .is_none_or(|native| a.get("agent_session") == Some(native))
                        })
                    })
                    .and_then(|a| a["agent_status"].as_str())
                    .unwrap_or("unavailable")
            };
            item(task, status)
        })
        .collect())
}

fn item(task: &Task, agent_status: &str) -> PickerItem {
    let kind = task
        .target
        .as_ref()
        .map(|t| t.kind.as_str())
        .unwrap_or("unassigned");
    let secondary = format!(
        "{} · task {} · agent {}",
        task.id,
        task.status.label(),
        agent_status
    );
    let mut preview = vec![
        format!("Task {} · generation {}", task.id, task.generation),
        format!("Task: {}    Agent: {agent_status}", task.status.label()),
        format!("Step: {}", task.step),
        format!("Goal: {}", task.request.goal),
        format!(
            "Worktree: {}",
            task.worktree.as_deref().unwrap_or("not created")
        ),
        format!("Branch: {}", task.branch),
        format!("Base: {}", task.base_commit),
        format!("Scope: {}", task.request.scope),
        format!("Acceptance: {}", task.request.acceptance),
        format!("Constraints: {}", task.request.constraints),
        format!("Decisions: {}", task.request.decisions),
        format!("Tried: {}", task.request.tried),
        format!("Open questions: {}", task.request.open_questions),
        format!("Expected result: {}", task.request.expected_result),
    ];
    if task.source_dirty {
        preview.push("Source was dirty: uncommitted changes were NOT copied.".into());
    }
    preview.extend(
        task.request
            .references
            .iter()
            .map(|p| format!("Reference (data): {p}")),
    );
    for c in &task.checkpoints {
        preview.extend([
            format!("Checkpoint: {}", c.progress),
            format!("Decisions: {}", c.decisions),
            format!("Tried: {}", c.tried),
            format!("Remaining: {}", c.remaining),
            format!("Next: {}", c.next_step),
            format!("Verification: {}", c.verification),
        ]);
    }
    if let Some(report) = &task.report {
        preview.push(format!("Result: {}", report.summary));
        preview.extend(report.tests.iter().map(|t| format!("Test: {t}")));
        preview.extend(
            report
                .artifacts
                .iter()
                .map(|t| format!("Artifact (data): {t}")),
        );
    }
    if let Some(e) = &task.evidence {
        preview.push(format!("Reported HEAD: {} · dirty: {}", e.head, e.dirty));
        preview.extend(e.changed_files.iter().map(|p| format!("Changed: {p}")));
    }
    PickerItem {
        id: task.id.clone(),
        primary: clean(&task.request.goal),
        secondary,
        trailing: Some(kind.into()),
        trailing_marker: None,
        document: Document::new(
            format!(
                "{} {} {} {}",
                task.id, task.request.goal, task.branch, task.request.repo
            ),
            &[
                ("status", task.status.label().into()),
                ("agent", kind.into()),
                ("repo", task.request.repo.clone()),
            ],
        ),
        preview: preview
            .into_iter()
            .flat_map(|s| s.lines().map(clean).collect::<Vec<_>>())
            .collect(),
        accent_slot: Some(
            match task.status {
                super::model::Status::Completed => "green",
                super::model::Status::Failed | super::model::Status::NeedsHelp => "yellow",
                _ => "mauve",
            }
            .into(),
        ),
    }
}

fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::model::Status;
    use crate::tasks::tests::{request, runner, Fixture};

    #[test]
    fn task_rows_separate_agent_status_from_reports_and_keep_filters() {
        let f = Fixture::new();
        let r = runner("idle");
        let e = f.engine(&r);
        let mut task = e.delegate(request(), "codex").unwrap();
        task.checkpoints.push(crate::tasks::tests::checkpoint());
        task.report = Some(crate::tasks::tests::report(Status::NeedsHelp));
        task.evidence = Some(crate::tasks::model::Evidence {
            head: "abc".into(),
            branch: "task".into(),
            dirty: true,
            changed_files: vec!["a.rs".into()],
        });
        let row = item(&task, "done");
        assert!(row.secondary.contains("task submitted · agent done"));
        assert!(row.preview.iter().any(|p| p.contains("NOT copied")));
        assert!(row
            .preview
            .iter()
            .any(|p| p.contains("Next: run render tests")));
        assert!(row.preview.iter().any(|p| p.contains("Changed: a.rs")));
        for (status, color) in [
            (Status::Completed, "green"),
            (Status::Failed, "yellow"),
            (Status::NeedsHelp, "yellow"),
        ] {
            task.status = status;
            assert_eq!(item(&task, "idle").accent_slot.as_deref(), Some(color));
        }
        assert_eq!(clean("a\x1bb\nc"), "abc");
        let mut cfg = Config::default();
        cfg.keys.insert(
            "tasks".into(),
            HashMap::from([("focus".into(), "ctrl-enter".into())]),
        );
        let mut mode = TasksMode::new(f.session.clone(), Arc::new(r), &cfg);
        assert_eq!(mode.key_bindings()["focus"], "ctrl-enter");
        mode.reload_config(&Config::default()).unwrap();
        assert!(mode.key_bindings().is_empty());
        crate::picker::assert_follows_prefix_concept("tasks", &mode.actions());
        let query =
            crate::query::CompiledQuery::compile("status:submitted agent:codex", &mode.schema())
                .unwrap();
        assert!(query
            .score(
                &row.document,
                &mut nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT)
            )
            .is_some());
    }

    #[test]
    fn background_refresh_has_one_worker_and_focus_revalidates() {
        let f = Fixture::new();
        let r = runner("idle");
        f.engine(&r).delegate(request(), "codex").unwrap();
        let r = r.on("agent list", r#"{"result":{"agents":[{"pane_id":"w2:p1","name":"swb-01234567-t00000001-g1","agent_status":"done","agent_session":{"kind":"id","value":"native-1"}}]}}"#)
            .on("agent focus", r#"{"result":{}}"#).on("workspace focus", r#"{"result":{}}"#);
        let mut mode = TasksMode::new(f.session.clone(), Arc::new(r), &Config::default());
        assert!(mode.initial().unwrap().is_empty());
        assert!(mode.is_polling());
        mode.refresh();
        for _ in 0..100 {
            if mode.poll().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!mode.is_polling());
        assert_eq!(mode.items.len(), 1);
        assert!(mode.items[0].secondary.contains("agent done"));
        assert!(matches!(
            mode.execute("t00000001", "focus").unwrap(),
            ActionOutcome::Close
        ));
        assert!(matches!(
            mode.execute("t00000001", "task_worktree").unwrap(),
            ActionOutcome::Close
        ));
        assert!(mode.execute("t00000001", "unknown").is_err());
        mode.next_refresh = Instant::now();
        mode.poll();
        assert!(mode.is_polling());
    }

    #[test]
    fn tasks_surface_can_close_at_narrow_and_wide_sizes() {
        use crate::surface::ScriptedHost;
        for width in [40, 80, 140] {
            let f = Fixture::new();
            let r = runner("idle");
            let task = f.engine(&r).delegate(request(), "codex").unwrap();
            let mut mode = TasksMode::new(f.session.clone(), Arc::new(r), &Config::default());
            mode.items = vec![item(&task, "working")];
            let mut host = ScriptedHost::new([ScriptedHost::key(KeyCode::Esc, KeyModifiers::NONE)]);
            host.size = (width, 20);
            picker::run_in(&mut host, mode, Theme::default(), Config::default()).unwrap();
        }
        let f = Fixture::new();
        let mode = TasksMode::new(
            f.session.clone(),
            Arc::new(crate::runner::MockRunner::new()),
            &Config::default(),
        );
        let mut host = ScriptedHost::new([ScriptedHost::key(KeyCode::Esc, KeyModifiers::NONE)]);
        picker::run_in(&mut host, mode, Theme::default(), Config::default()).unwrap();
    }

    #[test]
    fn broken_refresh_keeps_previous_rows_and_missing_agents_are_only_observations() {
        let f = Fixture::new();
        let r = runner("idle");
        let task = f.engine(&r).delegate(request(), "codex").unwrap();
        let mut mode = TasksMode::new(f.session.clone(), Arc::new(r), &Config::default());
        mode.items = vec![item(&task, "idle")];
        let (tx, rx) = mpsc::channel();
        drop(tx);
        mode.pending = Some(rx);
        assert!(mode.poll().unwrap().is_err());
        assert_eq!(mode.items.len(), 1);
        let (tx, rx) = mpsc::channel();
        tx.send(Err(anyhow::anyhow!("unreadable registry")))
            .unwrap();
        mode.pending = Some(rx);
        assert!(mode.poll().unwrap().is_err());
        assert_eq!(mode.items.len(), 1);
        let gone = crate::runner::MockRunner::new().failing("agent list");
        let rows = snapshot(&f.engine(&gone)).unwrap();
        assert!(rows[0].secondary.contains("unavailable"));
        let mut session = f.session.clone();
        session.stamp = "old".into();
        let rows = snapshot(&Engine {
            session: &session,
            runner: &gone,
        })
        .unwrap();
        assert!(rows[0].secondary.contains("server changed"));
        assert_eq!(
            f.session.store.read().unwrap().tasks[0].status,
            Status::Submitted
        );
        let empty = Fixture::new();
        assert!(snapshot(&empty.engine(&gone)).unwrap().is_empty());
        assert!(mode.execute("t00000009", "focus").is_err());
    }
}
