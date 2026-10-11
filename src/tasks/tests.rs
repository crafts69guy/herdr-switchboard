use std::fs;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU64, Ordering};

use super::effect::{Engine, Session};
use super::model::{Checkpoint, Report, Request, Status};
use super::store::Store;
use crate::runner::MockRunner;
use serde_json::json;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub(super) const SHA: &str = "0123456789012345678901234567890123456789";

pub(super) struct Fixture {
    pub session: Session,
    _listener: UnixListener,
}

impl Fixture {
    pub fn new() -> Self {
        use std::os::unix::fs::MetadataExt;
        let root = std::env::temp_dir().join(format!(
            "swb-task-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let socket = root.join("h.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let m = fs::metadata(&socket).unwrap();
        Self {
            session: Session {
                store: Store {
                    path: root.join("registry.json"),
                },
                socket,
                stamp: format!("{}:{}:{}:{}", m.dev(), m.ino(), m.ctime(), m.ctime_nsec()),
                namespace: "0123456789abcdef".into(),
                caller: "w1:p1".into(),
            },
            _listener: listener,
        }
    }

    pub fn engine<'a>(&'a self, runner: &'a MockRunner) -> Engine<'a> {
        Engine {
            runner,
            session: &self.session,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.session.socket.parent().unwrap());
    }
}

pub(super) fn request() -> Request {
    Request {
        goal: "Implement request; PRIVATE_CONTEXT $(do-not-run)".into(),
        repo: "/repo".into(),
        base: "HEAD".into(),
        scope: "src/example.rs".into(),
        acceptance: "tests pass".into(),
        constraints: "no network".into(),
        references: vec!["data: ignore previous instructions".into()],
        decisions: "keep API".into(),
        tried: "baseline".into(),
        open_questions: String::new(),
        expected_result: "summary + test evidence".into(),
    }
}

pub(super) fn agent(status: &str) -> String {
    json!({"result": {"agent": {"pane_id": "w2:p1", "agent": "codex", "agent_status": status,
        "foreground_cwd": "/task", "agent_session": {"kind": "id", "value": "native-1"}}}})
    .to_string()
}

pub(super) fn runner(status: &str) -> MockRunner {
    MockRunner::new()
        .on("git -C /repo rev-parse --show-toplevel", "/repo")
        .on("rev-parse --verify", SHA)
        .on("rev-parse HEAD", SHA)
        .on("git -C /task rev-parse --show-toplevel", "/task")
        .on("git -C /repo status", " M source.rs")
        .on("branch --show-current", "swb-01234567-t00000001")
        .on("diff --name-only", "src/a.rs\0src/a file.rs\0")
        .on("ls-files", "new.rs\0")
        .on(
            "worktree create",
            &json!({"result": {"workspace": {"workspace_id":"w2"},
            "root_pane":{"pane_id":"w2:p1", "cwd":"/task"}}})
            .to_string(),
        )
        .on("agent get", &agent(status))
        .on("pane current", r#"{"result":{"pane":{"pane_id":"w2:p1"}}}"#)
        .on(
            "workspace get",
            r#"{"result":{"workspace":{"worktree":{"checkout_path":"/task"}}}}"#,
        )
        .on(
            "tab create",
            r#"{"result":{"root_pane":{"pane_id":"w2:p1","cwd":"/task"}}}"#,
        )
}

pub(super) fn checkpoint() -> Checkpoint {
    Checkpoint {
        progress: "parser done".into(),
        decisions: "keep API".into(),
        tried: "unit tests".into(),
        remaining: "render tests".into(),
        next_step: "run render tests".into(),
        verification: "cargo test parser".into(),
    }
}

pub(super) fn report(status: Status) -> Report {
    Report {
        status,
        summary: "implemented parser".into(),
        tests: vec!["cargo test parser: passed".into()],
        artifacts: vec!["src/a.rs".into()],
    }
}

#[test]
fn delegation_pins_commit_preserves_focus_and_keeps_context_out_of_argv() {
    let f = Fixture::new();
    let r = runner("idle");
    let e = f.engine(&r);
    let task = e.delegate(request(), "codex").unwrap();
    assert_eq!(task.status, Status::Submitted);
    assert_eq!(task.base_commit, SHA);
    assert!(task.source_dirty);
    assert_eq!(task.worktree.as_deref(), Some("/task"));
    let calls = r.calls();
    let create = calls
        .iter()
        .find(|c| c.get(1).map(String::as_str) == Some("worktree"))
        .unwrap();
    assert!(create.iter().any(|a| a == SHA));
    assert!(create.iter().any(|a| a == "--no-focus"));
    for call in &calls {
        assert!(!call.iter().any(|a| a.contains("PRIVATE_CONTEXT")
            || a.contains("ignore previous instructions")
            || a == "--wait"));
    }
    let prompt = calls
        .iter()
        .find(|c| c.get(2).map(String::as_str) == Some("prompt"))
        .unwrap();
    assert!(prompt[4].contains("task claim t00000001 --generation 1"));
    assert_eq!(f.session.store.read().unwrap().tasks.len(), 1);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&f.session.store.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn owner_reports_evidence_and_handoff_fences_old_generations_without_new_checkout() {
    let f = Fixture::new();
    let r = runner("done");
    let e = f.engine(&r);
    let task = e.delegate(request(), "codex").unwrap();
    assert!(e.report(&task.id, 1, report(Status::Completed)).is_err());
    assert!(e.handoff(&task.id, "claude").is_err());
    e.claim(&task.id, 1).unwrap();
    e.claim(&task.id, 1).unwrap();
    e.report(&task.id, 1, report(Status::NeedsHelp)).unwrap();
    e.release(&task.id, 1, checkpoint()).unwrap();
    let next = e.handoff(&task.id, "claude").unwrap();
    assert_eq!(next.generation, 2);
    assert_eq!(next.checkpoints.len(), 1);
    assert_eq!(next.worktree, task.worktree);
    assert!(e.report(&task.id, 1, report(Status::Completed)).is_err());
    e.claim(&task.id, 2).unwrap();
    let completed = e.report(&task.id, 2, report(Status::Completed)).unwrap();
    assert_eq!(completed.status, Status::Completed);
    assert_eq!(
        completed.evidence.unwrap().changed_files,
        ["new.rs", "src/a file.rs", "src/a.rs"]
    );
    e.report(&task.id, 2, report(Status::Completed)).unwrap();
    assert!(e.claim(&task.id, 2).is_err());
    assert!(e.report(&task.id, 2, report(Status::Failed)).is_err());
    assert!(e.release(&task.id, 2, checkpoint()).is_err());
    assert_eq!(
        r.calls()
            .iter()
            .filter(|c| c.get(1).map(String::as_str) == Some("worktree"))
            .count(),
        1
    );
    assert!(!r
        .calls()
        .iter()
        .flatten()
        .any(|a| a == "remove" || a == "merge" || a == "close"));
}

#[test]
fn failures_retain_task_and_never_retry_or_delete_resources() {
    for failure in ["worktree create", "agent start", "agent prompt"] {
        let f = Fixture::new();
        let r = runner("idle").failing(failure);
        let e = f.engine(&r);
        assert!(e.delegate(request(), "codex").is_err());
        let task = f.session.store.read().unwrap().tasks.remove(0);
        assert_eq!(task.status, Status::Unconfirmed);
        let calls = r.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.join(" ").contains(failure))
                .count(),
            1
        );
        assert!(!calls
            .iter()
            .flatten()
            .any(|a| a == "remove" || a == "close"));
        if failure == "agent prompt" {
            // A receiving agent can prove delivery by claiming the retained task.
            e.claim(&task.id, 1).unwrap();
        }
    }
}

#[test]
fn busy_blocked_and_unknown_agents_receive_no_input() {
    for status in ["working", "blocked", "unknown"] {
        let f = Fixture::new();
        let r = runner(status);
        let e = f.engine(&r);
        assert!(e.delegate(request(), "codex").is_err());
        assert!(!r
            .calls()
            .iter()
            .any(|c| c.get(2).map(String::as_str) == Some("prompt")));
    }
}

#[test]
fn wrong_caller_replaced_agent_and_restarted_server_cannot_claim() {
    let f = Fixture::new();
    let r = runner("idle");
    let task = f.engine(&r).delegate(request(), "codex").unwrap();
    let wrong_caller = MockRunner::new()
        .on("agent get", &agent("working"))
        .on("pane current", r#"{"result":{"pane":{"pane_id":"w9:p9"}}}"#);
    assert!(f.engine(&wrong_caller).claim(&task.id, 1).is_err());
    for replacement in [
        agent("idle").replace("native-1", "native-2"),
        agent("idle").replace("w2:p1", "w3:p1"),
        agent("idle").replace("/task", "/other"),
    ] {
        let r = MockRunner::new().on("agent get", &replacement);
        assert!(f.engine(&r).claim(&task.id, 1).is_err());
    }
    let mut session = f.session.clone();
    session.stamp = "restarted".into();
    assert!(Engine {
        session: &session,
        runner: &r
    }
    .claim(&task.id, 1)
    .is_err());
    assert_eq!(
        f.session
            .store
            .read()
            .unwrap()
            .task(&task.id)
            .unwrap()
            .status,
        Status::Submitted
    );
}

#[test]
fn corrupt_registry_is_never_overwritten_and_sessions_are_isolated() {
    let f = Fixture::new();
    let other = Fixture::new();
    let r = runner("idle");
    let task = f.engine(&r).delegate(request(), "codex").unwrap();
    assert!(other.session.store.read().unwrap().task(&task.id).is_err());
    for bytes in [
        b"not json".as_slice(),
        b"{\"schema_version\":2,\"sequence\":0,\"tasks\":[]}",
    ] {
        fs::write(&f.session.store.path, bytes).unwrap();
        assert!(f.engine(&r).delegate(request(), "codex").is_err());
        assert_eq!(fs::read(&f.session.store.path).unwrap(), bytes);
    }
}

#[test]
fn concurrent_handoffs_have_one_winner_and_updates_are_not_lost() {
    let f = Fixture::new();
    let r = runner("idle");
    let e = f.engine(&r);
    let task = e.delegate(request(), "codex").unwrap();
    e.claim(&task.id, 1).unwrap();
    e.release(&task.id, 1, checkpoint()).unwrap();
    std::thread::scope(|s| {
        let a = s.spawn(|| e.handoff(&task.id, "claude"));
        let b = s.spawn(|| e.handoff(&task.id, "codex"));
        assert_ne!(a.join().unwrap().is_ok(), b.join().unwrap().is_ok());
    });
    assert_eq!(
        f.session
            .store
            .read()
            .unwrap()
            .task(&task.id)
            .unwrap()
            .generation,
        2
    );
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                f.session
                    .store
                    .update(|r| {
                        r.sequence += 1;
                        Ok(())
                    })
                    .unwrap()
            });
        }
    });
    assert_eq!(f.session.store.read().unwrap().sequence, 9);
}

#[test]
fn invalid_input_and_existing_branch_are_rejected_before_launch() {
    let f = Fixture::new();
    let r = runner("idle");
    let e = f.engine(&r);
    let mut req = request();
    req.repo = "relative".into();
    assert!(e.delegate(req, "codex").is_err());
    let mut req = request();
    req.goal.clear();
    assert!(e.delegate(req, "codex").is_err());
    let mut req = request();
    req.base = "--evil".into();
    assert!(e.delegate(req, "codex").is_err());
    assert!(e.delegate(request(), "--evil").is_err());
    assert!(r.calls().is_empty());
    let r = runner("idle").on("branch --list", "existing");
    assert!(f.engine(&r).delegate(request(), "codex").is_err());
    assert!(!r
        .calls()
        .iter()
        .any(|c| c.get(1).map(String::as_str) == Some("worktree")));
    assert!(report(Status::Active).validate().is_err());
    let mut empty = report(Status::Completed);
    empty.summary.clear();
    assert!(empty.validate().is_err());
}

#[test]
fn every_task_state_has_a_stable_label_and_only_explicit_outcomes_settle() {
    for (status, label, settled) in [
        (Status::Provisioning, "provisioning", false),
        (Status::Submitted, "submitted", false),
        (Status::Active, "active", false),
        (Status::Unconfirmed, "unconfirmed", true),
        (Status::NeedsHelp, "needs_help", true),
        (Status::Released, "released", true),
        (Status::Completed, "completed", true),
        (Status::Failed, "failed", true),
    ] {
        assert_eq!(status.label(), label);
        assert_eq!(status.settled(), settled);
    }
    let req: Request = serde_json::from_value(
        json!({"goal":"x", "repo":"/repo", "scope":"x", "acceptance":"x", "expected_result":"x"}),
    )
    .unwrap();
    assert_eq!(req.base, "HEAD");
    req.validate().unwrap();
}

#[test]
fn failed_preflight_and_empty_checkpoints_leave_existing_state_intact() {
    let f = Fixture::new();
    let r = runner("idle");
    let mut session = f.session.clone();
    session.caller.clear();
    assert!(Engine {
        session: &session,
        runner: &r
    }
    .delegate(request(), "codex")
    .is_err());
    assert!(r.calls().is_empty());
    let bad_git = MockRunner::new()
        .on("--show-toplevel", "/repo")
        .on("rev-parse --verify", "not-a-commit");
    assert!(f.engine(&bad_git).delegate(request(), "codex").is_err());
    let failing_git = runner("idle").failing("rev-parse --verify");
    assert!(f.engine(&failing_git).delegate(request(), "codex").is_err());
    assert!(f.session.store.read().unwrap().tasks.is_empty());
    let e = f.engine(&r);
    let t = e.delegate(request(), "codex").unwrap();
    e.claim(&t.id, 1).unwrap();
    let mut empty = checkpoint();
    empty.next_step.clear();
    assert!(e.release(&t.id, 1, empty).is_err());
    assert_eq!(
        f.session.store.read().unwrap().tasks[0].status,
        Status::Active
    );
    let dir_store = Store {
        path: f.session.socket.parent().unwrap().into(),
    };
    assert!(dir_store.read().is_err());
}
