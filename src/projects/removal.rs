//! Linked-worktree confirmation and Git operations; callers run IO on a worker.

use std::path::Path;

use anyhow::{ensure, Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::data::{parse_worktree_list, Entry, Kind};
use crate::runner::CommandRunner;

#[derive(Clone)]
pub(super) struct Snapshot {
    pub entry: Entry,
    pub path: String,
    pub common: String,
    pub head: String,
    pub branch: Option<String>,
    pub dirty: bool,
}

pub(super) struct Request {
    pub snapshot: Snapshot,
    pub force: bool,
    pub delete_branch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Control {
    Input,
    Force,
    Branch,
    Confirm,
    Cancel,
}

#[derive(Default)]
pub(super) struct State {
    pub entry: Option<Entry>,
    pub snapshot: Option<Snapshot>,
    pub confirmation: String,
    pub force: bool,
    pub delete_branch: bool,
    pub focus: usize,
    pub status: Option<&'static str>,
    pub error: Option<String>,
    pub zones: Vec<(Rect, Control)>,
}

impl State {
    pub fn open(&mut self, entry: Entry) {
        *self = Self {
            entry: Some(entry),
            status: Some("Checking worktree…"),
            ..Self::default()
        };
    }

    pub fn prepared(&mut self, result: Result<Snapshot, String>) {
        self.status = None;
        match result {
            Ok(snapshot) => self.snapshot = Some(snapshot),
            Err(error) => self.error = Some(error),
        }
    }

    pub fn expected(&self) -> String {
        let name = self.entry.as_ref().map_or("", |entry| entry.label.as_str());
        if self.force {
            format!("force {name}")
        } else {
            name.into()
        }
    }

    pub fn confirmed(&self) -> bool {
        self.status.is_none() && self.snapshot.is_some() && self.confirmation == self.expected()
    }

    pub fn begin(&mut self) -> Option<Request> {
        if !self.confirmed() {
            return None;
        }
        let snapshot = self.snapshot.clone()?;
        self.status = Some("Removing… please wait; this cannot be cancelled.");
        self.error = None;
        Some(Request {
            snapshot,
            force: self.force,
            delete_branch: self.delete_branch,
        })
    }

    pub fn activate(&mut self, control: Control) -> Option<Control> {
        if self.status.is_some() {
            return None;
        }
        match control {
            Control::Input => self.focus = 0,
            Control::Force => {
                self.focus = 1;
                self.force = !self.force;
                self.confirmation.clear();
            }
            Control::Branch => {
                if self.snapshot.as_ref().is_some_and(|s| s.branch.is_some()) {
                    self.focus = 2;
                    self.delete_branch = !self.delete_branch;
                }
            }
            Control::Confirm | Control::Cancel => return Some(control),
        }
        None
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Control> {
        if self.status.is_some() {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Some(Control::Cancel),
            KeyCode::Char('c') if ctrl => return Some(Control::Cancel),
            KeyCode::Enter => return Some(Control::Confirm),
            KeyCode::Tab | KeyCode::BackTab => {
                let fields = if self.snapshot.as_ref().is_some_and(|s| s.branch.is_some()) {
                    3
                } else {
                    2
                };
                let step = if key.code == KeyCode::BackTab {
                    fields - 1
                } else {
                    1
                };
                self.focus = (self.focus + step) % fields;
            }
            KeyCode::Char(' ') if self.focus != 0 => {
                return self.activate(if self.focus == 1 {
                    Control::Force
                } else {
                    Control::Branch
                });
            }
            KeyCode::Backspace if self.focus == 0 => {
                self.confirmation.pop();
            }
            KeyCode::Char('u') if ctrl && self.focus == 0 => self.confirmation.clear(),
            KeyCode::Char(c)
                if self.focus == 0 && !ctrl && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.confirmation.push(c);
            }
            _ => {}
        }
        None
    }
}

fn git(runner: &dyn CommandRunner, args: &[&str]) -> Result<String> {
    let output = runner.output("git", args).context("Could not run Git")?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).context("Git returned invalid UTF-8")
}

pub(super) fn prepare(runner: &dyn CommandRunner, entry: Entry) -> Result<Snapshot> {
    ensure!(entry.kind == Kind::Worktree, "Select a linked worktree.");
    let dir = entry.dir.as_deref().context("Worktree has no directory.")?;
    ensure!(
        Path::new(dir).is_absolute(),
        "Worktree path must be absolute."
    );
    let path = Path::new(dir)
        .canonicalize()
        .context("Worktree directory is missing.")?;
    let path = path
        .to_str()
        .context("Worktree path is not UTF-8.")?
        .to_string();
    let common = git(
        runner,
        &[
            "-C",
            &path,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ],
    )?;
    let common = Path::new(common.trim())
        .canonicalize()
        .context("Git directory is missing.")?;
    let common = common
        .to_str()
        .context("Git directory is not UTF-8.")?
        .to_string();
    let records = parse_worktree_list(&git(
        runner,
        &[
            "--git-dir",
            &common,
            "worktree",
            "list",
            "--porcelain",
            "-z",
        ],
    )?);
    let (index, record) = records
        .into_iter()
        .enumerate()
        .find(|(_, record)| {
            Path::new(&record.path).canonicalize().ok().as_deref() == Some(Path::new(&path))
        })
        .context("Worktree is no longer registered.")?;
    ensure!(index != 0, "The main worktree cannot be removed.");
    ensure!(
        !record.locked,
        "Worktree is locked; unlock it outside Switchboard first."
    );
    ensure!(!record.prunable, "Worktree metadata is stale.");
    let dirty = !git(
        runner,
        &[
            "-C",
            &path,
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )?
    .is_empty();
    Ok(Snapshot {
        entry,
        path,
        common,
        head: record.head,
        branch: record.branch,
        dirty,
    })
}

/// A branch failure is a warning: the worktree is already gone and must leave the list.
pub(super) fn remove(runner: &dyn CommandRunner, request: &Request) -> Result<Option<String>> {
    let snapshot = &request.snapshot;
    let fresh = prepare(runner, snapshot.entry.clone())?;
    ensure!(
        (
            fresh.path.as_str(),
            fresh.common.as_str(),
            fresh.head.as_str(),
            &fresh.branch
        ) == (
            snapshot.path.as_str(),
            snapshot.common.as_str(),
            snapshot.head.as_str(),
            &snapshot.branch
        ),
        "Worktree metadata changed. Cancel and reopen the confirmation."
    );
    let mut args = vec!["--git-dir", &snapshot.common, "worktree", "remove"];
    if request.force {
        args.push("--force");
    }
    args.extend(["--", &snapshot.path]);
    git(runner, &args)?;
    if request.delete_branch {
        if let Some(branch) = &snapshot.branch {
            let result = (|| {
                let reference = format!("refs/heads/{branch}");
                let head = git(
                    runner,
                    &[
                        "--git-dir",
                        &snapshot.common,
                        "rev-parse",
                        "--verify",
                        &reference,
                    ],
                )?;
                ensure!(
                    head.trim() == snapshot.head,
                    "Branch changed since confirmation."
                );
                git(
                    runner,
                    &["--git-dir", &snapshot.common, "branch", "-d", "--", branch],
                )?;
                Ok::<_, anyhow::Error>(())
            })();
            if let Err(error) = result {
                let message = error.to_string();
                let reason = message.lines().next().unwrap_or("Git refused deletion.");
                return Ok(Some(format!(
                    "Removed worktree; kept branch {branch}: {reason}"
                )));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{MockRunner, SystemRunner};
    use ratatui::style::Color;
    use std::path::PathBuf;

    struct Fixture {
        root: PathBuf,
        entry: Entry,
        common: String,
        main: String,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = crate::state::test_scratch()
                .unwrap()
                .join(format!("removal-{tag}"));
            std::fs::create_dir_all(root.join("main/.git")).unwrap();
            std::fs::create_dir_all(root.join("linked tree")).unwrap();
            let root = root.canonicalize().unwrap();
            let path = root.join("linked tree").to_str().unwrap().to_string();
            let entry = Entry {
                kind: Kind::Worktree,
                id: path.clone(),
                dir: Some(path),
                label: "linked tree".into(),
                primary: "repo".into(),
                secondary: "feature".into(),
                search: "repo feature worktree".into(),
                icon: String::new(),
                icon_color: Color::Reset,
            };
            Self {
                common: root.join("main/.git").to_str().unwrap().into(),
                main: root.join("main").to_str().unwrap().into(),
                root,
                entry,
            }
        }

        fn records(&self, extra: &str) -> String {
            format!("worktree {}\0HEAD main-head\0branch refs/heads/main\0\0worktree {}\0HEAD feature-head\0branch refs/heads/feature\0{extra}\0", self.main, self.entry.id)
        }

        fn runner(&self) -> MockRunner {
            MockRunner::new()
                .on("--git-common-dir", &self.common)
                .on("worktree list", &self.records(""))
                .on("rev-parse --verify", "feature-head\n")
        }

        fn snapshot(&self) -> Snapshot {
            prepare(&self.runner(), self.entry.clone()).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn confirmation_force_and_branch_are_explicit_and_reset_on_open() {
        let fixture = Fixture::new("confirmation");
        let mut state = State::default();
        state.open(fixture.entry.clone());
        assert!(state.begin().is_none());
        assert!(
            state.on_key(key(KeyCode::Esc)).is_none(),
            "loading owns input"
        );
        state.prepared(Ok(fixture.snapshot()));
        assert!(!state.force && !state.delete_branch);
        assert!(!state.confirmed());
        for c in "linked tree".chars() {
            state.on_key(key(KeyCode::Char(c)));
        }
        assert!(state.confirmed());
        state.on_key(key(KeyCode::Tab));
        state.on_key(key(KeyCode::Char(' ')));
        assert!(state.force);
        assert!(!state.confirmed());
        assert_eq!(state.expected(), "force linked tree");
        state.on_key(key(KeyCode::Tab));
        state.on_key(key(KeyCode::Char(' ')));
        assert!(state.delete_branch);
        state.on_key(key(KeyCode::Tab));
        assert_eq!(state.focus, 0);
        state.confirmation = "force linked tree".into();
        assert_eq!(state.on_key(key(KeyCode::Enter)), Some(Control::Confirm));
        let request = state.begin().unwrap();
        assert!(request.force && request.delete_branch);
        assert!(
            state.activate(Control::Cancel).is_none(),
            "removal cannot cancel"
        );
        state.open(fixture.entry.clone());
        assert!(state.confirmation.is_empty());
        assert!(!state.force && !state.delete_branch);
    }

    #[test]
    fn detached_confirmation_editing_and_cancel_controls_work() {
        let fixture = Fixture::new("detached-controls");
        let mut snapshot = fixture.snapshot();
        snapshot.branch = None;
        let mut state = State::default();
        state.open(fixture.entry.clone());
        state.prepared(Ok(snapshot));
        state.activate(Control::Branch);
        assert!(!state.delete_branch);
        state.on_key(key(KeyCode::BackTab));
        assert_eq!(state.focus, 1);
        state.activate(Control::Input);
        state.on_key(key(KeyCode::Char('x')));
        state.on_key(key(KeyCode::Backspace));
        assert!(state.confirmation.is_empty());
        state.confirmation = "wrong".into();
        state.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(state.confirmation.is_empty());
        state.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        state.on_key(key(KeyCode::Down));
        assert!(state.confirmation.is_empty());
        assert_eq!(state.on_key(key(KeyCode::Esc)), Some(Control::Cancel));
        assert_eq!(
            state.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Control::Cancel)
        );
        assert_eq!(state.activate(Control::Cancel), Some(Control::Cancel));
        assert_eq!(state.activate(Control::Confirm), Some(Control::Confirm));
        state.prepared(Err("missing".into()));
        assert_eq!(state.error.as_deref(), Some("missing"));
    }

    #[test]
    fn removal_uses_git_and_only_explicit_force_or_branch_deletion() {
        let fixture = Fixture::new("commands");
        for force in [false, true] {
            let runner = fixture.runner();
            let request = Request {
                snapshot: fixture.snapshot(),
                force,
                delete_branch: false,
            };
            assert!(remove(&runner, &request).unwrap().is_none());
            let calls = runner.calls();
            let removal = calls.last().unwrap();
            let mut expected = vec!["git", "--git-dir", &fixture.common, "worktree", "remove"];
            if force {
                expected.push("--force");
            }
            expected.extend(["--", &fixture.entry.id]);
            assert_eq!(removal, &expected);
            assert!(!calls.iter().any(|call| call[0] == "rm"));
        }
        let runner = fixture.runner();
        remove(
            &runner,
            &Request {
                snapshot: fixture.snapshot(),
                force: false,
                delete_branch: true,
            },
        )
        .unwrap();
        assert_eq!(
            runner.calls().last().unwrap(),
            &vec![
                "git",
                "--git-dir",
                &fixture.common,
                "branch",
                "-d",
                "--",
                "feature"
            ]
        );
    }

    #[test]
    fn invalid_locked_main_stale_and_changed_worktrees_never_remove() {
        let fixture = Fixture::new("guards");
        for extra in ["locked\0", "locked reason\0", "prunable stale\0"] {
            let runner = MockRunner::new()
                .on("--git-common-dir", &fixture.common)
                .on("worktree list", &fixture.records(extra));
            assert!(prepare(&runner, fixture.entry.clone()).is_err());
            assert!(!runner
                .calls()
                .iter()
                .any(|call| call.iter().any(|arg| arg == "remove")));
        }
        let main = Entry {
            dir: Some(fixture.main.clone()),
            ..fixture.entry.clone()
        };
        assert!(prepare(&fixture.runner(), main)
            .err()
            .unwrap()
            .to_string()
            .contains("main worktree"));
        for (kind, dir) in [
            (Kind::Repo, Some("/tmp")),
            (Kind::Worktree, None),
            (Kind::Worktree, Some("relative")),
            (Kind::Worktree, Some("/missing-worktree-directory")),
        ] {
            let entry = Entry {
                kind,
                dir: dir.map(str::to_string),
                ..fixture.entry.clone()
            };
            assert!(prepare(&fixture.runner(), entry).is_err());
        }
        let runner = MockRunner::new().on("--git-common-dir", &fixture.common);
        assert!(prepare(&runner, fixture.entry.clone()).is_err());
        let mut snapshot = fixture.snapshot();
        snapshot.head = "old-head".into();
        let runner = fixture.runner();
        assert!(remove(
            &runner,
            &Request {
                snapshot,
                force: true,
                delete_branch: true
            }
        )
        .unwrap_err()
        .to_string()
        .contains("metadata changed"));
        assert!(!runner
            .calls()
            .iter()
            .any(|call| call.iter().any(|arg| arg == "remove")));
    }

    #[test]
    fn dirty_and_git_errors_are_reported_and_branch_failure_is_partial_success() {
        let fixture = Fixture::new("failures");
        let runner = fixture.runner().on("status --porcelain", "?? new file\0");
        assert!(prepare(&runner, fixture.entry.clone()).unwrap().dirty);
        let request = Request {
            snapshot: fixture.snapshot(),
            force: false,
            delete_branch: true,
        };
        let runner = fixture
            .runner()
            .failing_with("worktree remove", "local changes");
        assert!(remove(&runner, &request)
            .unwrap_err()
            .to_string()
            .contains("local changes"));
        assert!(!runner
            .calls()
            .iter()
            .any(|call| call.iter().any(|arg| arg == "-d")));
        let runner = fixture
            .runner()
            .failing_with("branch -d", "not fully merged\nhint: use -D");
        let warning = remove(&runner, &request).unwrap().unwrap();
        assert!(warning.contains("not fully merged"));
        assert!(!warning.contains('\n'));
        assert!(!warning.contains("hint:"));
        let runner = MockRunner::new()
            .on("--git-common-dir", &fixture.common)
            .on("worktree list", &fixture.records(""))
            .on("rev-parse --verify", "changed");
        assert!(remove(&runner, &request)
            .unwrap()
            .unwrap()
            .contains("Branch changed"));
        assert!(!runner
            .calls()
            .iter()
            .any(|call| call.iter().any(|arg| arg == "-d")));
        let runner = fixture
            .runner()
            .failing_with("rev-parse", "broken repository");
        assert!(prepare(&runner, fixture.entry.clone()).is_err());
        let mut detached = fixture.snapshot();
        detached.branch = None;
        let records = fixture
            .records("")
            .replace("branch refs/heads/feature\0", "detached\0");
        let runner = MockRunner::new()
            .on("--git-common-dir", &fixture.common)
            .on("worktree list", &records);
        assert!(remove(
            &runner,
            &Request {
                snapshot: detached,
                force: false,
                delete_branch: true
            }
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn real_git_removes_clean_and_forced_worktrees_and_keeps_unmerged_branches() {
        let fixture = Fixture::new("real-git");
        let runner = SystemRunner;
        std::fs::remove_dir_all(&fixture.common).unwrap();
        git(
            &runner,
            &["init", "--template=", "-b", "main", &fixture.main],
        )
        .unwrap();
        git(
            &runner,
            &["-C", &fixture.main, "config", "core.hooksPath", "/dev/null"],
        )
        .unwrap();
        git(
            &runner,
            &[
                "-C",
                &fixture.main,
                "-c",
                "user.name=Switchboard Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        )
        .unwrap();
        std::fs::remove_dir_all(&fixture.entry.id).unwrap();
        git(
            &runner,
            &[
                "-C",
                &fixture.main,
                "worktree",
                "add",
                "-b",
                "feature",
                &fixture.entry.id,
            ],
        )
        .unwrap();
        let snapshot = prepare(&runner, fixture.entry.clone()).unwrap();
        git(
            &runner,
            &["-C", &fixture.main, "worktree", "lock", &fixture.entry.id],
        )
        .unwrap();
        assert!(prepare(&runner, fixture.entry.clone()).is_err());
        assert!(remove(
            &runner,
            &Request {
                snapshot: snapshot.clone(),
                force: true,
                delete_branch: false
            }
        )
        .is_err());
        git(
            &runner,
            &["-C", &fixture.main, "worktree", "unlock", &fixture.entry.id],
        )
        .unwrap();
        std::fs::write(Path::new(&fixture.entry.id).join("untracked"), "keep me").unwrap();
        assert!(prepare(&runner, fixture.entry.clone()).unwrap().dirty);
        assert!(remove(
            &runner,
            &Request {
                snapshot: snapshot.clone(),
                force: false,
                delete_branch: true
            }
        )
        .is_err());
        assert!(Path::new(&fixture.entry.id).exists());
        assert!(remove(
            &runner,
            &Request {
                snapshot,
                force: true,
                delete_branch: false
            }
        )
        .unwrap()
        .is_none());
        assert!(!Path::new(&fixture.entry.id).exists());
        git(
            &runner,
            &[
                "-C",
                &fixture.main,
                "rev-parse",
                "--verify",
                "refs/heads/feature",
            ],
        )
        .unwrap();
        git(
            &runner,
            &[
                "-C",
                &fixture.main,
                "worktree",
                "add",
                &fixture.entry.id,
                "feature",
            ],
        )
        .unwrap();
        let snapshot = prepare(&runner, fixture.entry.clone()).unwrap();
        assert!(remove(
            &runner,
            &Request {
                snapshot,
                force: false,
                delete_branch: true
            }
        )
        .unwrap()
        .is_none());
        assert!(git(
            &runner,
            &[
                "-C",
                &fixture.main,
                "rev-parse",
                "--verify",
                "refs/heads/feature"
            ]
        )
        .is_err());
        git(
            &runner,
            &[
                "-C",
                &fixture.main,
                "worktree",
                "add",
                "-b",
                "unmerged",
                &fixture.entry.id,
            ],
        )
        .unwrap();
        git(
            &runner,
            &[
                "-C",
                &fixture.entry.id,
                "-c",
                "user.name=Switchboard Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "unmerged",
            ],
        )
        .unwrap();
        let snapshot = prepare(&runner, fixture.entry.clone()).unwrap();
        assert!(remove(
            &runner,
            &Request {
                snapshot,
                force: false,
                delete_branch: true
            }
        )
        .unwrap()
        .unwrap()
        .contains("kept branch"));
        assert!(!Path::new(&fixture.entry.id).exists());
        git(
            &runner,
            &[
                "-C",
                &fixture.main,
                "rev-parse",
                "--verify",
                "refs/heads/unmerged",
            ],
        )
        .unwrap();
        let list = git(
            &runner,
            &["-C", &fixture.main, "worktree", "list", "--porcelain", "-z"],
        )
        .unwrap();
        assert_eq!(parse_worktree_list(&list).len(), 1);
    }
}
