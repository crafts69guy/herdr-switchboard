//! URL entry and the non-interactive ghq clone effect, private to Projects.

use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::{bail, ensure, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::Paragraph,
    Frame,
};
use regex::Regex;

use crate::{data::Entry, runner::CommandRunner};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Control {
    Submit,
    Cancel,
}

#[derive(Default)]
pub(super) struct State {
    pub input: String,
    cursor: usize,
    pub error: Option<String>,
    pub status: Option<&'static str>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub close_after_cancel: bool,
    pub zones: Vec<(Rect, Control)>,
    pub completed: Option<Entry>,
    pub worker: Option<std::thread::JoinHandle<()>>,
}

impl State {
    pub fn begin(&mut self, status: &'static str) -> Arc<AtomicBool> {
        self.error = None;
        self.status = Some(status);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        cancel
    }

    pub fn finish(&mut self) -> bool {
        let cancelled = self
            .cancel
            .take()
            .is_some_and(|c| c.load(Ordering::Acquire));
        self.status = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        cancelled
    }

    pub fn paste(&mut self, text: &str) {
        if self.cancel.is_none() && !text.chars().any(char::is_control) {
            self.input.insert_str(self.cursor, text);
            self.cursor += text.len();
            self.error = None;
        } else if self.cancel.is_none() {
            self.error = Some("Paste a single repository URL without control characters.".into());
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Control> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
            self.close_after_cancel = ctrl;
            return Some(Control::Cancel);
        }
        if self.cancel.is_some() {
            return None;
        }
        match key.code {
            KeyCode::Enter => return Some(Control::Submit),
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Left => {
                self.cursor = self.input[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i)
            }
            KeyCode::Right => {
                self.cursor += self.input[self.cursor..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                let before = self.input[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i);
                self.input.replace_range(before..self.cursor, "");
                self.cursor = before;
            }
            KeyCode::Delete if self.cursor < self.input.len() => {
                self.input.remove(self.cursor);
            }
            KeyCode::Char(c)
                if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) && !c.is_control() =>
            {
                self.paste(&c.to_string())
            }
            _ => {}
        }
        None
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Validate without querying the network; ghq retains authority over URL resolution.
pub(super) fn reference(input: &str) -> Result<String> {
    let input = input.trim();
    ensure!(!input.is_empty(), "Enter a repository URL.");
    ensure!(
        !input.chars().any(|c| c.is_whitespace() || c.is_control()),
        "Repository URLs cannot contain whitespace or control characters."
    );
    let pattern = Regex::new(
        r"^(?:(https?://)([A-Za-z0-9.-]+(?::[0-9]+)?)/|(ssh://)(?:[A-Za-z0-9_.-]+@)?([A-Za-z0-9.-]+(?::[0-9]+)?)/|(?:[A-Za-z0-9_.-]+@)?([A-Za-z0-9.-]+):)([A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*)/?$",
    )?;
    let captures = pattern.captures(input);
    let path = if let Some(c) = &captures {
        c.get(6).map_or("", |m| m.as_str())
    } else {
        ensure!(
            !input.contains(':')
                && !input.contains('@')
                && !input.contains('?')
                && !input.contains('#'),
            "Use a clone URL without embedded credentials, query, or fragment."
        );
        input.trim_end_matches('/')
    };
    let parts: Vec<_> = path.split('/').collect();
    ensure!(
        parts.len() >= 2
            && parts.iter().all(|s| !s.is_empty()
                && *s != "."
                && *s != ".."
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))),
        "Use owner/repo, host/namespace/repo, or a full clone URL."
    );
    ensure!(
        !parts.contains(&"-"),
        "Use the repository clone URL, not a GitLab page link."
    );
    if captures.is_none() && parts.len() >= 3 && parts[0].contains('.') {
        return reference(&format!("https://{input}"));
    }
    let host = captures
        .as_ref()
        .and_then(|c| c.get(2).or_else(|| c.get(4)).or_else(|| c.get(5)))
        .map(|m| {
            m.as_str()
                .split(':')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        });
    if matches!(host.as_deref(), Some("github.com" | "bitbucket.org")) {
        ensure!(
            parts.len() == 2,
            "Use the repository clone URL, not a page link."
        );
    }
    ensure!(
        !parts.last().is_some_and(|p| *p == ".git"),
        "Repository name is missing."
    );
    Ok(input.trim_end_matches('/').into())
}

fn canonical_reference(runner: &dyn CommandRunner, input: &str) -> Result<String> {
    let reference = reference(input)?;
    if reference.contains(":") {
        return Ok(reference);
    }
    let host = runner
        .capture("git", &["config", "--get", "ghq.defaultHost"])
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "github.com".into());
    self::reference(&format!("https://{host}/{reference}"))
}

pub(super) fn seed(runner: &dyn CommandRunner, cancel: &AtomicBool) -> Option<String> {
    let programs: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbpaste", &[])]
    } else {
        &[
            ("wl-paste", &["--no-newline"]),
            ("xclip", &["-o", "-selection", "clipboard"]),
        ]
    };
    for (program, args) in programs {
        if let Ok(output) = runner.output_controlled(program, args, &[], cancel) {
            if output.status.success() {
                return reference(String::from_utf8_lossy(&output.stdout).trim()).ok();
            }
        }
    }
    None
}

pub(super) fn run(
    runner: &dyn CommandRunner,
    input: &str,
    cancel: &AtomicBool,
) -> Result<(String, String)> {
    let url = canonical_reference(runner, input)?;
    let ssh = std::env::var("GIT_SSH_COMMAND")
        .ok()
        .or_else(|| {
            std::env::var("GIT_SSH")
                .ok()
                .map(|s| format!("'{}'", s.replace('\'', "'\\''")))
        })
        .or_else(|| runner.capture("git", &["config", "--get", "core.sshCommand"]))
        .unwrap_or_else(|| "ssh".into());
    let ssh = format!("{ssh} -o BatchMode=yes");
    let environment = [
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ASKPASS", ""),
        ("SSH_ASKPASS", "/usr/bin/false"),
        ("SSH_ASKPASS_REQUIRE", "force"),
        ("GCM_INTERACTIVE", "never"),
        ("GIT_SSH_COMMAND", ssh.as_str()),
    ];
    let output = runner
        .output_controlled(
            "ghq",
            &["get", "--vcs", "git", "--", &url],
            &environment,
            cancel,
        )
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("ghq is required. Install ghq and retry.")
            } else {
                anyhow::anyhow!("Clone could not finish. Check Git authentication and retry.")
            }
        })?;
    // External output may contain credential-helper diagnostics; never draw or log it.
    ensure!(output.status.success(), "Clone failed. Check the URL, network, SSH agent or Git credential helper. Configure authentication outside Switchboard, then retry.");
    if cancel.load(Ordering::Acquire) {
        bail!("Clone cancelled.");
    }
    let paths = runner
        .capture("ghq", &["list", "--exact", "--full-path", "--", &url])
        .ok_or_else(|| anyhow::anyhow!("Cloned, but could not find the repository path."))?;
    let ids = runner
        .capture("ghq", &["list", "--exact", "--", &url])
        .ok_or_else(|| anyhow::anyhow!("Cloned, but could not identify the repository."))?;
    let paths: Vec<_> = paths.lines().collect();
    let ids: Vec<_> = ids.lines().collect();
    ensure!(
        paths.len() == 1
            && ids.len() == 1
            && Path::new(paths[0]).is_absolute()
            && Path::new(paths[0]).is_dir(),
        "Cloned, but the repository path is missing or ambiguous. Check ghq configuration."
    );
    Ok((ids[0].into(), paths[0].into()))
}

pub(super) fn draw(f: &mut Frame, app: &mut super::App, area: Rect) {
    let popup = crate::tui::centered(area, 86, 12);
    app.background.paint(f, popup);
    let text = app.theme.or("text", Color::Reset);
    let sub = app.theme.or("subtext0", Color::DarkGray);
    let border = app.theme.or("overlay0", Color::DarkGray);
    let frame = crate::tui::boxed("Clone repository", app.title_color, border);
    let inner = frame.inner(popup);
    f.render_widget(frame, popup);
    let (body, bar) = crate::tui::reserve_bar(inner, 1);
    let (body, feedback) = crate::tui::reserve_bar(body, 3);
    let state = &mut app.clone;
    let input = format!(
        "{}▏{}",
        &state.input[..state.cursor],
        &state.input[state.cursor..]
    );
    // Keep the cursor visible even when a URL is wider than the card.
    let offset = input[..state.cursor]
        .chars()
        .count()
        .saturating_sub(body.width.saturating_sub(3) as usize);
    let input: String = input.chars().skip(offset).collect();
    f.render_widget(
        Paragraph::new(vec![
            Line::from("GitHub · GitLab · Bitbucket · self-hosted Git"),
            Line::from("owner/repo · host/namespace/repo · HTTPS / SSH URL"),
            Line::from(""),
            Line::from(input),
            Line::from(""),
        ])
        .style(Style::default().fg(text)),
        body,
    );
    f.render_widget(
        Paragraph::new(
            state
                .status
                .or(state.error.as_deref())
                .unwrap_or("Uses your configured Git credentials or SSH agent."),
        )
        .style(Style::default().fg(if state.error.is_some() {
            app.theme.or("red", Color::Red)
        } else {
            sub
        }))
        .wrap(ratatui::widgets::Wrap { trim: false }),
        feedback,
    );
    let pills = [
        crate::tui::Pill {
            key: "enter",
            label: "clone",
            color: app.theme.or("green", Color::Green),
        },
        crate::tui::Pill {
            key: "esc",
            label: if state.cancel.is_some() {
                "cancel"
            } else {
                "back"
            },
            color: sub,
        },
    ];
    let (spans, zones) =
        crate::tui::pill_row(&pills, app.theme.or("panel_bg", Color::Black), bar.x);
    f.render_widget(Paragraph::new(Line::from(spans)), bar);
    state.zones = if bar.height > 0 {
        zones
            .into_iter()
            .zip([Control::Submit, Control::Cancel])
            .map(|((start, end), control)| {
                let start = start.min(bar.right());
                let end = end.min(bar.right());
                (Rect::new(start, bar.y, end - start, 1), control)
            })
            .collect()
    } else {
        Vec::new()
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    #[test]
    fn accepts_clone_references_with_nested_namespaces_and_ssh() {
        for input in [
            "owner/repo",
            "https://github.com/o/r.git/",
            "gitlab.com/team/sub/repo",
            "https://gitlab.example.com/team/sub/repo",
            "git@bitbucket.org:o/r.git",
            "ssh://git@git.example.com:2222/team/sub/repo.git",
            "git@work:team/repo",
        ] {
            assert!(reference(input).is_ok(), "{input}");
        }
        assert_eq!(
            reference("  gitlab.com/team/sub/repo.git/ ").unwrap(),
            "https://gitlab.com/team/sub/repo.git"
        );
        assert_eq!(
            canonical_reference(
                &MockRunner::new().on("ghq.defaultHost", "gitlab.com"),
                "team/repo"
            )
            .unwrap(),
            "https://gitlab.com/team/repo"
        );
        assert_eq!(
            canonical_reference(&MockRunner::new(), "owner/repo").unwrap(),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn refuses_credentials_page_links_local_paths_and_unsafe_input() {
        for input in [
            "",
            "/tmp/repo",
            "../repo",
            "file:///tmp/repo",
            "ext::command",
            "https://token@github.com/o/r",
            "https://user:secret@gitlab.com/o/r",
            "https://github.com/o/r?token=secret",
            "https://github.com/o/r#branch",
            "github.com/o/r/tree/main",
            "git@github.com:o/r/pull/1",
            "https://gitlab.com/o/r/-/tree/main",
            "o/../r",
            "o/repo\nother/repo",
            "o/\x1br",
            "https://gitlab.com/o/.git",
        ] {
            assert!(reference(input).is_err(), "{input}");
        }
    }

    #[test]
    fn input_edits_at_unicode_boundaries_and_cannot_submit_twice() {
        let mut state = State::default();
        state.paste("répo");
        state.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        state.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(state.input, "réo");
        state.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        state.on_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(state.input, "éo");
        state.paste("\nsecret");
        assert_eq!(state.input, "éo");
        assert!(state.error.is_some());
        state.begin("Cloning…");
        assert!(state
            .on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .is_none());
        state.paste("ignored");
        assert_eq!(state.input, "éo");
    }

    #[test]
    fn typing_navigation_clear_and_drop_work_without_leaving_a_worker() {
        let mut state = State::default();
        for key in [
            KeyCode::Char('a'),
            KeyCode::Char('b'),
            KeyCode::Home,
            KeyCode::Right,
            KeyCode::End,
            KeyCode::Tab,
        ] {
            state.on_key(KeyEvent::new(key, KeyModifiers::NONE));
        }
        assert_eq!(state.input, "ab");
        assert_eq!(
            state.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Control::Submit)
        );
        state.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(state.input.is_empty());
        let cancel = state.begin("Cloning…");
        let observed = Arc::clone(&cancel);
        state.worker = Some(std::thread::spawn(move || {
            while !observed.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }));
        drop(state);
        assert!(cancel.load(Ordering::Acquire));
    }

    #[test]
    fn clone_resolves_exact_host_and_path_and_never_updates() {
        let path = crate::state::test_scratch()
            .unwrap()
            .join("clone-success/gitlab.com/team/sub/repo");
        std::fs::create_dir_all(&path).unwrap();
        let runner = MockRunner::new()
            .on("ghq list --exact --full-path", path.to_str().unwrap())
            .on("ghq list --exact --", "gitlab.com/team/sub/repo");
        let result = run(
            &runner,
            "gitlab.com/team/sub/repo.git",
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            result,
            (
                "gitlab.com/team/sub/repo".into(),
                path.to_string_lossy().into_owned()
            )
        );
        let calls = runner.calls();
        assert!(calls.contains(&vec![
            "ghq".into(),
            "get".into(),
            "--vcs".into(),
            "git".into(),
            "--".into(),
            "https://gitlab.com/team/sub/repo.git".into()
        ]));
        assert!(!calls.iter().flatten().any(|a| a == "-u" || a == "--update"));
        let environments = runner.environments.lock().unwrap();
        assert!(environments[0].contains(&("GIT_TERMINAL_PROMPT".into(), "0".into())));
        assert!(environments[0]
            .iter()
            .any(|(key, value)| key == "GIT_SSH_COMMAND" && value.ends_with("-o BatchMode=yes")));
    }

    #[test]
    fn clone_errors_do_not_expose_stderr_and_ambiguous_paths_are_refused() {
        let runner = MockRunner::new().failing_with("ghq get", "https://secret@example.com/o/r");
        let error = run(&runner, "owner/repo", &AtomicBool::new(false))
            .unwrap_err()
            .to_string();
        assert!(error.contains("authentication") || error.contains("credential"));
        assert!(!error.contains("secret"));
        assert!(!runner
            .calls()
            .iter()
            .any(|c| c.get(1).is_some_and(|a| a == "list")));
        for paths in ["", "/one/host/o/r\n/two/host/o/r", "/missing/host/o/r"] {
            let runner = MockRunner::new()
                .on("ghq list --exact --full-path", paths)
                .on("ghq list --exact --", "host/o/r");
            assert!(run(&runner, "https://host/o/r", &AtomicBool::new(false)).is_err());
        }
        assert!(run(&MockRunner::new(), "o/r", &AtomicBool::new(true)).is_err());
    }

    #[test]
    fn clipboard_prefill_only_accepts_a_valid_reference() {
        for (text, expected) in [
            ("https://gitlab.com/o/r\n", Some("https://gitlab.com/o/r")),
            ("personal notes", None),
            ("https://token@gitlab.com/o/r", None),
        ] {
            let runner = MockRunner::new()
                .on("pbpaste", text)
                .on("wl-paste", text)
                .on("xclip", text);
            assert_eq!(seed(&runner, &AtomicBool::new(false)).as_deref(), expected);
        }
    }
}
