//! Explicit managed-install updates. The worker owns IO; the hosted surface owns
//! every terminal cell, including failures after a release has been installed.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use ansi_to_tui::IntoText;
use anyhow::{anyhow, bail, Context, Result};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use serde::Deserialize;

use crate::data::{Config, Theme};
use crate::runner::{CommandRunner, SystemRunner};
use crate::surface::{Surface, Transition};
use crate::tui::{self, Pill, SurfaceBackground};
use crate::{notify::Notifier, state, update};

const REPO: &str = "crafts69guy/herdr-switchboard";
const REMOTE: &str = "https://github.com/crafts69guy/herdr-switchboard";
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Checking,
    Installing,
    Preparing,
}

impl Stage {
    fn index(self) -> usize {
        match self {
            Self::Checking => 0,
            Self::Installing => 1,
            Self::Preparing => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Updated,
    Current,
    Blocked,
    Warning,
    Failed,
}

struct Outcome {
    status: Status,
    title: String,
    message: String,
    details: String,
}

impl Outcome {
    fn new(status: Status, title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            title: title.into(),
            message: message.into(),
            details: String::new(),
        }
    }

    fn failed(error: &anyhow::Error) -> Self {
        Self {
            status: Status::Failed,
            title: "Update failed.".into(),
            message: clean(&error.to_string()),
            details: clean(&format!("{error:#}")),
        }
    }
}

enum Completion {
    Stage(Stage, Option<String>),
    Done(Result<Outcome>),
}

struct Worker {
    inbox: mpsc::Receiver<Completion>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // An in-flight installation must finish even if rendering fails.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Job {
    runner: Arc<dyn CommandRunner + Send + Sync>,
    herdr: String,
    root: PathBuf,
    version: String,
    lock: PathBuf,
}

impl Job {
    fn for_current_process() -> Result<Self> {
        Ok(Self {
            runner: Arc::new(SystemRunner),
            herdr: env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into()),
            root: env::var_os("HERDR_PLUGIN_ROOT")
                .map(PathBuf::from)
                .unwrap_or_default(),
            version: VERSION.into(),
            lock: state::state_file("install")
                .context("No Switchboard state directory is available")?,
        })
    }

    fn start(self, notifier: Notifier) -> Result<Worker> {
        let (sender, inbox) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("switchboard-update".into())
            .spawn(move || {
                let result = self.perform(&sender);
                let notification = match &result {
                    Ok(outcome) if outcome.status == Status::Updated => {
                        Some((outcome.title.as_str(), "done"))
                    }
                    Ok(outcome) if outcome.status == Status::Warning => {
                        Some((outcome.title.as_str(), "request"))
                    }
                    Err(_) => Some(("Switchboard update failed.", "request")),
                    _ => None,
                };
                if let Some((body, sound)) = notification {
                    notifier.send_message(body, sound);
                }
                let _ = sender.send(Completion::Done(result));
            })?;
        Ok(Worker {
            inbox,
            thread: Some(thread),
        })
    }

    fn command(&self, program: &str, args: &[&str], root: &Path) -> Result<String> {
        let root = root.to_str().context("Plugin path is not valid UTF-8")?;
        let output = self.runner.output_controlled(
            program,
            args,
            &[
                ("HERDR_PLUGIN_ROOT", root),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("GIT_SSH_COMMAND", "ssh -oBatchMode=yes"),
                // Child launchers must not send another notification on failure.
                ("SWITCHBOARD_BIN", ""),
            ],
            &AtomicBool::new(false),
        )?;
        if !output.status.success() {
            bail!(
                "{program} exited with {}.\n{}",
                output.status,
                clean(&String::from_utf8_lossy(if output.stderr.is_empty() {
                    &output.stdout
                } else {
                    &output.stderr
                }))
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn installation(&self) -> Result<Installation> {
        let text = self
            .command(
                &self.herdr,
                &["plugin", "list", "--plugin", "switchboard", "--json"],
                &self.root,
            )
            .context("Could not read Switchboard's installation")?;
        installation(&text)
    }

    fn perform(&self, sender: &mpsc::Sender<Completion>) -> Result<Outcome> {
        let installed = self.installation()?;
        if installed.source.kind != "github" {
            let mut outcome = Outcome::new(
                Status::Blocked,
                "Update skipped.",
                "This is not a managed GitHub install.",
            );
            if installed.source.kind == "local" {
                outcome.title = "Linked checkout — update skipped.".into();
                outcome.message = "Pull and rebuild your checkout, then relink it in Herdr.".into();
                outcome.details = format!(
                    "Checkout: {}\n\nRun git pull and cargo build --release in that checkout.\nThen run herdr plugin unlink switchboard and herdr plugin link with the checkout path.\nSwitchboard has not changed any files.",
                    clean(&installed.plugin_root.display().to_string())
                );
            }
            return Ok(outcome);
        }
        installed.validate_root(&self.root)?;
        let Some(_lock) = state::try_lock_private(&self.lock)? else {
            return Ok(Outcome::new(
                Status::Blocked,
                "Update already running.",
                "Another Switchboard updater is active. Wait for it to finish.",
            ));
        };
        let tags = self
            .command(
                "git",
                &["ls-remote", "--tags", "--refs", REMOTE],
                &self.root,
            )
            .context("Could not check the latest stable release")?;
        let release = update::latest_release(&tags).context("No stable release tag was found")?;
        let _ = sender.send(Completion::Stage(
            Stage::Checking,
            Some(release.version.clone()),
        ));
        let current =
            update::parse_version(&self.version).context("Installed version is invalid")?;
        let latest =
            update::parse_version(&release.version).context("Release version is invalid")?;
        if current >= latest {
            return Ok(Outcome::new(
                Status::Current,
                if current == latest {
                    "Already up to date."
                } else {
                    "This install is newer than the latest stable release."
                },
                "No files were changed.",
            ));
        }
        let _ = sender.send(Completion::Stage(Stage::Installing, None));
        self.command(
            &self.herdr,
            &["plugin", "install", REPO, "--yes", "--ref", &release.name],
            &self.root,
        )
        .context("Could not install the release; inspect Details before trying again")?;

        let _ = sender.send(Completion::Stage(Stage::Preparing, None));
        let preparation = (|| -> Result<()> {
            let installed = self.installation()?;
            installed.validate_root(&self.root)?;
            anyhow::ensure!(
                installed.version == release.version,
                "Herdr reports a different installed version"
            );
            let picker = installed.plugin_root.join("bin/picker.sh");
            let picker = picker.to_str().context("Picker path is not valid UTF-8")?;
            // One invocation prepares and verifies the new binary using its own policy.
            let version = self.command("bash", &[picker, "--version"], &installed.plugin_root)?;
            anyhow::ensure!(
                version.trim() == format!("herdr-switchboard {}", release.version),
                "The prepared binary reports a different version"
            );
            Ok(())
        })();
        if let Err(error) = preparation {
            let mut outcome = Outcome::new(
                Status::Warning,
                "Release installed; binary preparation failed.",
                "Reopen Switchboard to retry preparing its binary.",
            );
            outcome.details = clean(&format!("{error:#}"));
            return Ok(outcome);
        }
        Ok(Outcome::new(
            Status::Updated,
            format!("Updated to v{}.", release.version),
            "Reopen Switchboard to use it. Open Changelog to see what changed.",
        ))
    }
}

#[derive(Deserialize)]
struct Installation {
    plugin_id: String,
    plugin_root: PathBuf,
    version: String,
    source: Source,
}

#[derive(Deserialize)]
struct Source {
    kind: String,
    owner: Option<String>,
    repo: Option<String>,
    subdir: Option<String>,
    managed_path: Option<PathBuf>,
}

impl Installation {
    fn validate_root(&self, launch_root: &Path) -> Result<()> {
        anyhow::ensure!(
            self.source.kind == "github"
                && self.source.owner.as_deref() == Some("crafts69guy")
                && self.source.repo.as_deref() == Some("herdr-switchboard")
                && self.source.subdir.as_deref().is_none_or(str::is_empty),
            "The registered source is not the Switchboard repository"
        );
        let managed = self
            .source
            .managed_path
            .as_ref()
            .context("The managed checkout path is missing")?
            .canonicalize()
            .context("The managed checkout is unavailable")?;
        anyhow::ensure!(
            managed == self.plugin_root.canonicalize()? && managed == launch_root.canonicalize()?,
            "The registered checkout differs from the checkout running this updater"
        );
        Ok(())
    }
}

fn installation(text: &str) -> Result<Installation> {
    #[derive(Deserialize)]
    struct Response {
        result: Plugins,
    }
    #[derive(Deserialize)]
    struct Plugins {
        plugins: Vec<Installation>,
    }
    let mut plugins = serde_json::from_str::<Response>(text)
        .context("Herdr returned unreadable installation metadata")?
        .result
        .plugins;
    anyhow::ensure!(
        plugins.len() == 1,
        "Expected exactly one Switchboard install"
    );
    let plugin = plugins.pop().context("Switchboard is not installed")?;
    anyhow::ensure!(
        plugin.plugin_id == "switchboard",
        "Herdr returned another plugin"
    );
    Ok(plugin)
}

/// Parse terminal escapes rather than passing subprocess control bytes to the backend.
fn clean(text: &str) -> String {
    let bounded: String = text.chars().take(64 * 1024).collect();
    let parsed = bounded.as_bytes().into_text().unwrap_or_default();
    parsed
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .flat_map(|span| span.content.chars())
                .filter(|c| !c.is_control() || *c == '\t')
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct App {
    theme: Theme,
    background: SurfaceBackground,
    title_color: Color,
    version: String,
    target: Option<String>,
    stage: Stage,
    outcome: Option<Outcome>,
    job: Option<Job>,
    worker: Option<Worker>,
    frame: usize,
    details: bool,
    scroll: u16,
    max_scroll: u16,
    bar_row: u16,
    zones: Vec<(u16, u16, KeyCode)>,
    notifier: Notifier,
    redraw_pending: bool,
}

impl App {
    fn new(cfg: Config, theme: Theme, job: Job) -> Self {
        Self {
            background: SurfaceBackground::resolve(&theme, cfg.common.transparency),
            title_color: theme
                .resolve(&cfg.common.title_color)
                .unwrap_or(Color::Yellow),
            theme,
            version: job.version.clone(),
            target: None,
            stage: Stage::Checking,
            outcome: None,
            job: Some(job),
            worker: None,
            frame: 0,
            details: false,
            scroll: 0,
            max_scroll: 0,
            bar_row: 0,
            zones: Vec::new(),
            notifier: Notifier::new(&cfg),
            redraw_pending: false,
        }
    }

    fn finish(&mut self, outcome: Outcome) {
        self.outcome = Some(outcome);
        self.redraw_pending = true;
    }

    fn drain_worker(&mut self) {
        while let Some(worker) = &self.worker {
            match worker.inbox.try_recv() {
                Ok(Completion::Stage(stage, target)) => {
                    self.stage = stage;
                    if target.is_some() {
                        self.target = target;
                    }
                    self.redraw_pending = true;
                }
                Ok(Completion::Done(result)) => {
                    self.worker.take();
                    self.finish(result.unwrap_or_else(|error| Outcome::failed(&error)));
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker.take();
                    self.finish(Outcome::failed(&anyhow!(
                        "The update worker stopped unexpectedly"
                    )));
                    break;
                }
            }
        }
    }

    fn key(&mut self, key: KeyCode, modifiers: KeyModifiers) -> Transition<()> {
        if self.outcome.is_none() {
            return Transition::Wait;
        }
        let close = matches!(key, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q'))
            || (key == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL));
        if self.details {
            if matches!(key, KeyCode::Esc | KeyCode::Char('d')) {
                self.details = false;
            } else if close {
                return Transition::Exit(());
            } else {
                self.scroll = match key {
                    KeyCode::Down | KeyCode::Char('j') => self.scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => self.scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => self.scroll.saturating_add(10),
                    KeyCode::PageUp => self.scroll.saturating_sub(10),
                    KeyCode::Home | KeyCode::Char('g') => 0,
                    KeyCode::End | KeyCode::Char('G') => self.max_scroll,
                    _ => return Transition::Wait,
                }
                .min(self.max_scroll);
            }
        } else if close {
            return Transition::Exit(());
        } else if key == KeyCode::Char('d')
            && self.outcome.as_ref().is_some_and(|o| !o.details.is_empty())
        {
            self.details = true;
            self.scroll = 0;
        } else {
            return Transition::Wait;
        }
        Transition::Redraw
    }
}

impl Surface for App {
    type Output = ();

    fn draw(&mut self, frame: &mut Frame) {
        draw(frame, self);
    }

    fn after_draw(&mut self) -> Result<()> {
        if let Some(job) = self.job.take() {
            match job.start(self.notifier.clone()) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => self.finish(Outcome::failed(&error)),
            }
        }
        Ok(())
    }

    fn on_event(&mut self, event: Event) -> Result<Transition<()>> {
        self.drain_worker();
        // A held close key must not swallow the result before its first frame.
        if self.outcome.is_some() && self.redraw_pending {
            return Ok(Transition::Redraw);
        }
        let transition = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.key(key.code, key.modifiers),
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let at = Position::new(mouse.column, mouse.row);
                    match tui::zone_at(&self.zones, self.bar_row, at) {
                        Some(key) => self.key(key, KeyModifiers::NONE),
                        None => Transition::Wait,
                    }
                }
                MouseEventKind::ScrollDown if self.details => {
                    self.scroll = self.scroll.saturating_add(3).min(self.max_scroll);
                    Transition::Redraw
                }
                MouseEventKind::ScrollUp if self.details => {
                    self.scroll = self.scroll.saturating_sub(3);
                    Transition::Redraw
                }
                _ => Transition::Wait,
            },
            _ => Transition::Wait,
        };
        Ok(
            if self.redraw_pending && matches!(transition, Transition::Wait) {
                Transition::Redraw
            } else {
                transition
            },
        )
    }

    fn tick_rate(&self) -> Duration {
        Duration::from_millis(if self.outcome.is_none() { 100 } else { 250 })
    }

    fn on_tick(&mut self) -> Result<Transition<()>> {
        if self.outcome.is_some() {
            return Ok(if self.redraw_pending {
                Transition::Redraw
            } else {
                Transition::Wait
            });
        }
        self.drain_worker();
        self.frame = self.frame.wrapping_add(1);
        Ok(Transition::Redraw)
    }
}

fn draw(frame: &mut Frame, app: &mut App) {
    app.redraw_pending = false;
    app.background.paint(frame, frame.area());
    let card = tui::centered(frame.area(), 72, if app.details { 24 } else { 16 });
    let border = app.theme.or("overlay0", Color::DarkGray);
    let block = tui::boxed("Update Switchboard", app.title_color, border);
    // On tiny terminals reserve usable cells for status and the way out.
    let area = if card.height >= 4 && card.width >= 20 {
        let inner = block.inner(card);
        frame.render_widget(block, card);
        inner
    } else {
        card
    };
    let (body, bar) = tui::reserve_bar(area, 1);
    let text = app.theme.or("text", Color::Reset);
    let sub = app.theme.or("subtext0", Color::Gray);
    let green = app.theme.or("green", Color::Green);
    let accent = app.theme.or("teal", Color::Cyan);
    let color = match app.outcome.as_ref().map(|o| o.status) {
        Some(Status::Failed) => app.theme.or("red", Color::Red),
        Some(Status::Warning | Status::Blocked) => app.theme.or("yellow", Color::Yellow),
        Some(Status::Updated | Status::Current) => green,
        None => accent,
    };
    let target = app
        .target
        .as_ref()
        .map(|v| format!("v{v}"))
        .unwrap_or_else(|| {
            if app.outcome.is_some() {
                "not checked"
            } else {
                "checking…"
            }
            .into()
        });
    let version = format!("Installed v{}  →  Latest stable {target}", app.version);
    let mut lines = Vec::new();
    if app.details {
        if let Some(outcome) = &app.outcome {
            for line in outcome.details.lines() {
                for chunk in wrap_cells(line, body.width) {
                    lines.push(Line::styled(chunk, Style::default().fg(text)));
                }
            }
        }
        app.max_scroll = u16::try_from(lines.len())
            .unwrap_or(u16::MAX)
            .saturating_sub(body.height);
        app.scroll = app.scroll.min(app.max_scroll);
    } else {
        let status = app
            .outcome
            .as_ref()
            .map(|o| o.title.as_str())
            .unwrap_or(match app.stage {
                Stage::Checking => "Checking installation and latest release…",
                Stage::Installing => "Installing release…",
                Stage::Preparing => "Preparing binary…",
            });
        if body.height >= 9 {
            lines.push(Line::raw(""));
            lines.push(Line::styled(version, Style::default().fg(sub)));
            lines.push(Line::raw(""));
            const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
            let completed = app
                .outcome
                .as_ref()
                .is_some_and(|o| o.status == Status::Updated);
            for (index, label) in [
                "Check installation and latest release",
                "Install release",
                "Prepare binary",
            ]
            .iter()
            .enumerate()
            {
                let done = completed
                    || index < app.stage.index()
                    || (index == 0
                        && app
                            .outcome
                            .as_ref()
                            .is_some_and(|o| o.status == Status::Current));
                let active = index == app.stage.index() && app.outcome.is_none();
                let failed = index == app.stage.index()
                    && app
                        .outcome
                        .as_ref()
                        .is_some_and(|o| matches!(o.status, Status::Warning | Status::Failed));
                let symbol = if done {
                    "✓"
                } else if active {
                    SPINNER[app.frame % SPINNER.len()]
                } else if failed {
                    "!"
                } else if app.outcome.is_some() {
                    "—"
                } else {
                    "·"
                };
                let ink = if done {
                    green
                } else if active {
                    accent
                } else if failed {
                    color
                } else {
                    sub
                };
                lines.push(Line::styled(
                    format!("{symbol} {label}"),
                    Style::default().fg(ink),
                ));
            }
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            status.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        let message = app.outcome.as_ref().map(|o| o.message.as_str()).unwrap_or(
            if app.stage == Stage::Preparing {
                "Downloading a verified binary, or building locally if needed."
            } else {
                "Please keep this pane open until the update finishes."
            },
        );
        for chunk in wrap_cells(message, body.width) {
            lines.push(Line::styled(chunk, Style::default().fg(sub)));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).scroll((if app.details { app.scroll } else { 0 }, 0)),
        body,
    );
    app.zones.clear();
    app.bar_row = bar.y;
    if bar.height == 0 {
        return;
    }
    if app.outcome.is_none() {
        frame.render_widget(
            Paragraph::new("Updating… close is available when finished.")
                .style(Style::default().fg(sub)),
            bar,
        );
    } else {
        let mut actions = vec![(
            Pill::new("esc", if app.details { "back" } else { "close" }, color),
            KeyCode::Esc,
        )];
        if !app.details && app.outcome.as_ref().is_some_and(|o| !o.details.is_empty()) {
            actions.push((Pill::new("d", "details", accent), KeyCode::Char('d')));
        }
        // Omit pills that would be clipped, together with their hit zones.
        let mut width = 1usize;
        actions.retain(|(pill, _)| {
            width += pill.key.chars().count() + pill.label.chars().count() + 4;
            width <= bar.width as usize
        });
        if actions.is_empty() {
            frame.render_widget(Paragraph::new("esc").style(Style::default().fg(color)), bar);
            app.zones
                .push((bar.x, bar.x.saturating_add(bar.width.min(3)), KeyCode::Esc));
        } else {
            let pills = actions
                .iter()
                .map(|(p, _)| Pill::new(p.key, p.label, p.color))
                .collect::<Vec<_>>();
            let (spans, zones) =
                tui::pill_row(&pills, app.theme.or("panel_bg", Color::Black), bar.x);
            app.zones = zones
                .into_iter()
                .zip(actions.iter().map(|(_, key)| *key))
                .map(|((a, b), key)| (a, b, key))
                .collect();
            frame.render_widget(Paragraph::new(Line::from(spans)), bar);
        }
    }
}

/// Hard-wrap diagnostics too: a single long URL/path must remain readable.
fn wrap_cells(text: &str, width: u16) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for row in text.split('\n') {
        let mut line = String::new();
        let mut cells = 0;
        for c in row.chars() {
            let mut buffer = [0; 4];
            let w = Span::raw(&*c.encode_utf8(&mut buffer)).width() as u16;
            if cells + w > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                cells = 0;
            }
            line.push(c);
            cells += w;
        }
        lines.push(line);
    }
    lines
}

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    crate::surface::run(&mut App::new(cfg, theme, Job::for_current_process()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Transparency;
    use crate::runner::MockRunner;
    use ratatui::{backend::TestBackend, Terminal};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    const TAGS: &str = "a\trefs/tags/v9.9.8\nb\trefs/tags/v9.9.9\nc\trefs/tags/v10.0.0-rc.1\n";

    struct Fixture {
        dir: PathBuf,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = env::temp_dir().join(format!(
                "swb-self-update-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let root = dir.join("plugin");
            fs::create_dir_all(root.join("bin")).unwrap();
            Self { dir, root }
        }

        fn json(&self, kind: &str) -> String {
            serde_json::json!({
                "result": {"plugins": [{
                    "plugin_id": "switchboard",
                    "plugin_root": self.root,
                    "version": "9.9.9",
                    "source": {
                        "kind": kind, "owner": "crafts69guy", "repo": "herdr-switchboard",
                        "managed_path": self.root
                    }
                }]}
            })
            .to_string()
        }

        fn runner(&self) -> MockRunner {
            MockRunner::new()
                .on("plugin list", &self.json("github"))
                .on("ls-remote", TAGS)
                .on("picker.sh --version", "herdr-switchboard 9.9.9\n")
        }

        fn job(&self, runner: Arc<dyn CommandRunner + Send + Sync>) -> Job {
            Job {
                runner,
                herdr: "fake-herdr".into(),
                root: self.root.clone(),
                version: "1.0.0".into(),
                lock: self.dir.join("state/install"),
            }
        }

        fn perform(&self, runner: Arc<dyn CommandRunner + Send + Sync>) -> Result<Outcome> {
            self.job(runner).perform(&mpsc::channel().0)
        }

        fn app(&self) -> App {
            let mut cfg = Config::default();
            cfg.common.notifications = false;
            App::new(cfg, Theme::default(), self.job(Arc::new(self.runner())))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn screen(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn contents(buffer: &ratatui::buffer::Buffer) -> String {
        buffer.content.iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn startup_uses_private_state_without_creating_or_locking_it() {
        let job = Job::for_current_process().unwrap();
        assert_eq!(job.version, VERSION);
        assert!(job.lock.starts_with(state::test_scratch().unwrap()));
        assert!(!job.lock.exists());
        assert!(!job.lock.with_file_name(".install.lock").exists());
    }

    #[test]
    fn only_verified_managed_installations_reach_the_installer() {
        let fixture = Fixture::new();
        for json in [
            String::new(),
            "garbage".into(),
            "{}".into(),
            r#"{"result":{"plugins":[]}}"#.into(),
            fixture.json("local"),
            fixture.json("unknown"),
            fixture
                .json("github")
                .replace("crafts69guy", "another-owner"),
            fixture
                .json("github")
                .replace("switchboard\"", "different-plugin\""),
            fixture
                .json("github")
                .replace("managed_path", "missing_path"),
        ] {
            let runner = Arc::new(MockRunner::new().on("plugin list", &json));
            let result = fixture.perform(runner.clone());
            assert!(result.is_err() || result.unwrap().status == Status::Blocked);
            assert!(!runner
                .calls()
                .iter()
                .any(|c| c.iter().any(|a| a == "install")));
            assert_eq!(runner.calls().len(), 1);
        }
        let parsed: serde_json::Value = serde_json::from_str(&fixture.json("github")).unwrap();
        let pretty = serde_json::to_string_pretty(&parsed).unwrap();
        installation(&pretty)
            .unwrap()
            .validate_root(&fixture.root)
            .unwrap();
        assert!(installation(&pretty)
            .unwrap()
            .validate_root(&fixture.dir)
            .is_err());
    }

    #[test]
    fn an_update_pins_the_latest_stable_ref_and_verifies_the_new_binary() {
        let fixture = Fixture::new();
        let runner = Arc::new(fixture.runner());
        let (sender, receiver) = mpsc::channel();
        let result = fixture.job(runner.clone()).perform(&sender).unwrap();
        assert_eq!(result.status, Status::Updated);
        assert_eq!(result.title, "Updated to v9.9.9.");
        let calls = runner.calls();
        assert_eq!(calls.len(), 5);
        assert_eq!(
            calls[2],
            [
                "fake-herdr",
                "plugin",
                "install",
                REPO,
                "--yes",
                "--ref",
                "v9.9.9"
            ]
        );
        assert_eq!(calls[4][0], "bash");
        assert_eq!(calls[4][2], "--version");
        let stages: Vec<_> = receiver
            .try_iter()
            .filter_map(|c| match c {
                Completion::Stage(stage, _) => Some(stage),
                _ => None,
            })
            .collect();
        assert_eq!(
            stages,
            [Stage::Checking, Stage::Installing, Stage::Preparing]
        );
        let environments = runner.environments.lock().unwrap();
        assert!(environments.iter().all(|env| env
            .iter()
            .any(|(k, v)| k == "GIT_TERMINAL_PROMPT" && v == "0")));
    }

    #[test]
    fn offline_and_tag_failures_never_fall_back_to_head() {
        let fixture = Fixture::new();
        for runner in [
            fixture.runner().failing("ls-remote"),
            MockRunner::new()
                .on("plugin list", &fixture.json("github"))
                .on("ls-remote", "a\trefs/tags/nightly\n"),
        ] {
            let runner = Arc::new(runner);
            assert!(fixture.perform(runner.clone()).is_err());
            assert_eq!(runner.calls().len(), 2);
        }
    }

    #[test]
    fn current_and_ahead_versions_do_not_install_or_downgrade() {
        let fixture = Fixture::new();
        for version in ["9.9.9", "10.0.0"] {
            let runner = Arc::new(fixture.runner());
            let mut job = fixture.job(runner.clone());
            job.version = version.into();
            let outcome = job.perform(&mpsc::channel().0).unwrap();
            assert_eq!(outcome.status, Status::Current);
            assert_eq!(runner.calls().len(), 2);
        }
    }

    #[test]
    fn install_failure_and_preparation_failure_are_distinct() {
        let fixture = Fixture::new();
        let runner = Arc::new(
            fixture
                .runner()
                .failing_with("plugin install", "network unavailable"),
        );
        let error = fixture.perform(runner.clone()).err().unwrap();
        assert!(format!("{error:#}").contains("network unavailable"));
        assert_eq!(runner.calls().len(), 3);
        for runner in [
            fixture
                .runner()
                .failing_with("picker.sh --version", "cargo unavailable"),
            MockRunner::new()
                .on("plugin list", &fixture.json("github"))
                .on("ls-remote", TAGS)
                .on("picker.sh --version", "herdr-switchboard 0.1.0"),
        ] {
            let outcome = fixture.perform(Arc::new(runner)).unwrap();
            assert_eq!(outcome.status, Status::Warning);
            assert!(outcome.title.contains("Release installed"));
            assert!(!outcome.details.is_empty());
        }
    }

    #[test]
    fn contention_stops_a_second_update_without_waiting_or_network_work() {
        let fixture = Fixture::new();
        let runner = Arc::new(fixture.runner());
        let job = fixture.job(runner.clone());
        let lock = state::try_lock_private(&job.lock).unwrap().unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            job.perform(&mpsc::channel().0).unwrap().status,
            Status::Blocked
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(runner.calls().len(), 1);
        drop(lock);
        assert_eq!(
            job.perform(&mpsc::channel().0).unwrap().status,
            Status::Updated
        );
    }

    #[test]
    fn the_first_frame_precedes_io_and_busy_input_never_closes() {
        let fixture = Fixture::new();
        let runner = Arc::new(fixture.runner());
        let mut app = fixture.app();
        app.job = Some(fixture.job(runner.clone()));
        let first = contents(&screen(&mut app, 80, 24));
        assert!(first.contains("Checking installation"));
        assert!(runner.calls().is_empty());
        for key in [
            KeyCode::Esc,
            KeyCode::Char('q'),
            KeyCode::Enter,
            KeyCode::Char('c'),
        ] {
            assert!(matches!(
                app.key(key, KeyModifiers::CONTROL),
                Transition::Wait
            ));
        }
        assert_eq!(app.tick_rate(), Duration::from_millis(100));
        app.after_draw().unwrap();
        while app.outcome.is_none() {
            app.on_tick().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(app.outcome.as_ref().unwrap().status, Status::Updated);
        assert_eq!(app.tick_rate(), Duration::from_millis(250));
        screen(&mut app, 80, 24);
        assert!(matches!(app.on_tick().unwrap(), Transition::Wait));
        assert!(matches!(
            app.key(KeyCode::Esc, KeyModifiers::NONE),
            Transition::Exit(())
        ));
    }

    #[test]
    fn details_scroll_and_escape_returns_to_the_result() {
        let fixture = Fixture::new();
        let mut app = fixture.app();
        let mut outcome = Outcome::new(
            Status::Warning,
            "Release installed; binary preparation failed.",
            "Reopen Switchboard to retry.",
        );
        outcome.details = (0..80).map(|i| format!("diagnostic {i}\n")).collect();
        app.finish(outcome);
        assert!(matches!(
            app.key(KeyCode::Char('d'), KeyModifiers::NONE),
            Transition::Redraw
        ));
        screen(&mut app, 80, 24);
        app.key(KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.scroll, app.max_scroll);
        assert!(contents(&screen(&mut app, 80, 24)).contains("diagnostic 79"));
        assert!(matches!(
            app.key(KeyCode::Esc, KeyModifiers::NONE),
            Transition::Redraw
        ));
        assert!(!app.details);
        assert!(contents(&screen(&mut app, 80, 24)).contains("Release installed"));
        assert!(matches!(
            app.key(KeyCode::Enter, KeyModifiers::NONE),
            Transition::Exit(())
        ));
    }

    #[test]
    fn each_running_stage_and_error_result_is_visible_without_child_output() {
        let fixture = Fixture::new();
        let mut app = fixture.app();
        for (stage, label) in [
            (Stage::Checking, "Checking installation"),
            (Stage::Installing, "Installing release"),
            (Stage::Preparing, "Preparing binary"),
        ] {
            app.stage = stage;
            let buffer = screen(&mut app, 80, 24);
            assert!(contents(&buffer).contains(label));
            assert!(app.zones.is_empty());
        }
        let (sender, inbox) = mpsc::channel();
        app.worker = Some(Worker {
            inbox,
            thread: None,
        });
        sender
            .send(Completion::Done(Err(anyhow!(
                "Installer failed: \x1b[31moffline\x1b[0m"
            ))))
            .unwrap();
        app.on_tick().unwrap();
        let buffer = screen(&mut app, 80, 24);
        assert!(contents(&buffer).contains("Update failed."));
        assert_eq!(app.outcome.as_ref().unwrap().status, Status::Failed);
        assert!(!app.outcome.as_ref().unwrap().details.contains('\x1b'));
        assert!(matches!(
            app.key(KeyCode::Char('d'), KeyModifiers::NONE),
            Transition::Redraw
        ));
        assert!(contents(&screen(&mut app, 80, 24)).contains("offline"));
        assert!(matches!(
            app.key(KeyCode::Char('q'), KeyModifiers::NONE),
            Transition::Exit(())
        ));
    }

    #[test]
    fn failed_workers_and_pending_work_keep_the_surface_responsive() {
        let fixture = Fixture::new();
        let mut app = fixture.app();
        let (sender, inbox) = mpsc::channel();
        app.worker = Some(Worker {
            inbox,
            thread: None,
        });
        let started = std::time::Instant::now();
        assert!(matches!(app.on_tick().unwrap(), Transition::Redraw));
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(matches!(
            app.on_event(Event::Resize(40, 12)).unwrap(),
            Transition::Wait
        ));
        screen(&mut app, 40, 12);
        assert!(app.outcome.is_none());
        drop(sender);
        app.on_tick().unwrap();
        assert!(app
            .outcome
            .as_ref()
            .unwrap()
            .details
            .contains("worker stopped"));
        app.finish(Outcome::failed(&anyhow!(
            "Could not start the update worker"
        )));
        assert!(matches!(app.on_tick().unwrap(), Transition::Redraw));
        screen(&mut app, 80, 24);
        assert!(matches!(app.on_tick().unwrap(), Transition::Wait));
        app.job.take();
        app.after_draw().unwrap();
        screen(&mut app, 0, 0);
        assert!(app.zones.is_empty());
    }

    #[test]
    fn worker_completion_preserves_errors_and_partial_success() {
        let fixture = Fixture::new();
        for (runner, expected) in [
            (fixture.runner().failing("plugin install"), Status::Failed),
            (
                fixture.runner().failing("picker.sh --version"),
                Status::Warning,
            ),
        ] {
            let worker = fixture
                .job(Arc::new(runner))
                .start(Notifier::silent())
                .unwrap();
            loop {
                match worker.inbox.recv_timeout(Duration::from_secs(3)).unwrap() {
                    Completion::Done(result) => {
                        let outcome = result.unwrap_or_else(|e| Outcome::failed(&e));
                        assert_eq!(outcome.status, expected);
                        assert!(!outcome.details.is_empty());
                        break;
                    }
                    Completion::Stage(_, _) => {}
                }
            }
            drop(worker);
            assert!(state::try_lock_private(&fixture.dir.join("state/install"))
                .unwrap()
                .is_some());
        }
    }

    #[test]
    fn queued_input_cannot_starve_completion_or_dismiss_an_unpainted_result() {
        use crossterm::event::KeyEvent;
        let fixture = Fixture::new();
        let mut app = fixture.app();
        let (sender, inbox) = mpsc::channel();
        app.worker = Some(Worker {
            inbox,
            thread: None,
        });
        let close = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        sender
            .send(Completion::Stage(Stage::Installing, None))
            .unwrap();
        assert!(matches!(
            app.on_event(close.clone()).unwrap(),
            Transition::Redraw
        ));
        assert_eq!(app.stage, Stage::Installing);
        screen(&mut app, 80, 24);
        sender
            .send(Completion::Done(Ok(Outcome::new(
                Status::Updated,
                "Updated.",
                "Reopen Switchboard.",
            ))))
            .unwrap();
        for _ in 0..3 {
            assert!(matches!(
                app.on_event(close.clone()).unwrap(),
                Transition::Redraw
            ));
        }
        assert!(contents(&screen(&mut app, 80, 24)).contains("Updated."));
        assert!(matches!(app.on_event(close).unwrap(), Transition::Exit(())));
    }

    #[test]
    fn keyboard_and_mouse_details_navigation_follow_the_drawn_footer() {
        use crossterm::event::{KeyEvent, MouseEvent};
        let fixture = Fixture::new();
        let mut app = fixture.app();
        let mut outcome = Outcome::failed(&anyhow!("Build failed"));
        outcome.details = (0..100).map(|n| format!("Line {n}\n")).collect();
        app.finish(outcome);
        screen(&mut app, 80, 24);
        let mouse = |kind, column, row| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        assert!(matches!(
            app.on_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0))
                .unwrap(),
            Transition::Wait
        ));
        let details = app
            .zones
            .iter()
            .find(|(_, _, key)| *key == KeyCode::Char('d'))
            .unwrap()
            .0;
        assert!(matches!(
            app.on_event(mouse(
                MouseEventKind::Down(MouseButton::Left),
                details,
                app.bar_row
            ))
            .unwrap(),
            Transition::Redraw
        ));
        screen(&mut app, 80, 24);
        for (key, expected) in [
            (KeyCode::Down, 1),
            (KeyCode::Up, 0),
            (KeyCode::PageDown, 10),
            (KeyCode::PageUp, 0),
            (KeyCode::End, app.max_scroll),
            (KeyCode::Home, 0),
        ] {
            app.on_event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)))
                .unwrap();
            assert_eq!(app.scroll, expected);
        }
        app.on_event(mouse(MouseEventKind::ScrollDown, 20, 10))
            .unwrap();
        assert_eq!(app.scroll, 3);
        app.on_event(mouse(MouseEventKind::ScrollUp, 20, 10))
            .unwrap();
        assert_eq!(app.scroll, 0);
        assert!(matches!(
            app.on_event(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE
            )))
            .unwrap(),
            Transition::Wait
        ));
        assert!(matches!(
            app.on_event(mouse(MouseEventKind::Moved, 20, 10)).unwrap(),
            Transition::Wait
        ));
        app.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(
            app.on_event(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE
            )))
            .unwrap(),
            Transition::Wait
        ));
        screen(&mut app, 80, 24);
        let close = app.zones[0].0;
        assert!(matches!(
            app.on_event(mouse(
                MouseEventKind::Down(MouseButton::Left),
                close,
                app.bar_row
            ))
            .unwrap(),
            Transition::Exit(())
        ));
    }

    #[test]
    fn all_sizes_keep_the_footer_and_background_contract() {
        let fixture = Fixture::new();
        for mode in [Transparency::Transparent, Transparency::Opaque] {
            for (w, h) in [(120, 40), (80, 24), (40, 12), (20, 4), (3, 2), (1, 1)] {
                let mut cfg = Config::default();
                cfg.common.notifications = false;
                cfg.common.transparency = mode;
                let theme = Theme::from_slots(&[("panel_bg", "#101214"), ("overlay0", "#333333")]);
                let mut app = App::new(cfg, theme, fixture.job(Arc::new(fixture.runner())));
                app.finish(Outcome::new(
                    Status::Updated,
                    "Updated.",
                    "Reopen Switchboard.",
                ));
                let buffer = screen(&mut app, w, h);
                let expected = if mode == Transparency::Opaque {
                    Color::Rgb(16, 18, 20)
                } else {
                    Color::Reset
                };
                assert_eq!(buffer[(0, 0)].bg, expected, "{w}x{h}");
                assert!(!app.zones.is_empty(), "{w}x{h}");
                assert!(app.zones.iter().all(|(a, b, _)| *a < *b && *b <= w));
                assert!(app.bar_row < h);
                if w >= 3 {
                    assert!(contents(&buffer).contains("esc"));
                }
            }
        }
    }

    #[test]
    fn finished_and_blocked_cards_do_not_look_like_pending_work() {
        let fixture = Fixture::new();
        for (status, stage, target) in [
            (Status::Current, Stage::Checking, Some("1.0.0")),
            (Status::Blocked, Stage::Checking, None),
            (Status::Warning, Stage::Preparing, Some("9.9.9")),
            (Status::Failed, Stage::Installing, Some("9.9.9")),
        ] {
            let mut app = fixture.app();
            app.stage = stage;
            app.target = target.map(str::to_string);
            app.finish(Outcome::new(status, "Finished.", "Read the result."));
            let text = contents(&screen(&mut app, 80, 24));
            assert!(text.contains("Finished."));
            if status == Status::Blocked {
                assert!(text.contains("Latest stable not checked"));
            }
            if status == Status::Current {
                assert!(text.contains("✓ Check installation"));
                assert!(text.contains("— Install release"));
            }
            if status == Status::Warning {
                assert!(text.contains("! Prepare binary"));
            }
            if status == Status::Failed {
                assert!(text.contains("! Install release"));
            }
        }
    }

    #[test]
    fn diagnostics_strip_control_sequences_and_wrap_wide_paths() {
        assert_eq!(clean("\x1b[31mfailed\x1b[0m\nnext\u{7}"), "failed\nnext");
        assert!(!clean("\x1b]52;c;YWJj\x07hello").contains('\x1b'));
        assert!(clean(&"x".repeat(100_000)).len() <= 64 * 1024);
        assert_eq!(wrap_cells("界界abc", 4), ["界界", "abc"]);
        assert!(wrap_cells("text", 0).is_empty());
        assert_eq!(wrap_cells("abcdefgh", 3), ["abc", "def", "gh"]);
    }

    #[test]
    fn replacing_the_managed_root_uses_the_new_picker_and_reaps_commands() {
        let fixture = Fixture::new();
        let herdr = fixture.dir.join("herdr");
        let json = fixture.json("github");
        let root = fixture.root.display();
        let script = format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \"$2\" == list ]]; then\nprintf '%s\\n' '{json}'\nelse\nrm -rf '{root}'\nmkdir -p '{root}/bin'\nprintf '%s\\n' '#!/usr/bin/env bash' 'printf \"herdr-switchboard 9.9.9\\\\n\"' > '{root}/bin/picker.sh'\nfi\n"
        );
        fs::write(&herdr, script).unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        // Only the tag query is canned; install really replaces a disposable root.
        struct Runner {
            herdr: String,
        }
        impl CommandRunner for Runner {
            fn output_controlled(
                &self,
                program: &str,
                args: &[&str],
                env: &[(&str, &str)],
                cancel: &AtomicBool,
            ) -> std::io::Result<std::process::Output> {
                if program == "git" {
                    MockRunner::new()
                        .on("ls-remote", TAGS)
                        .output(program, args)
                } else {
                    SystemRunner.output_controlled(
                        if program == "fake-herdr" {
                            &self.herdr
                        } else {
                            program
                        },
                        args,
                        env,
                        cancel,
                    )
                }
            }
            fn output(&self, _: &str, _: &[&str]) -> std::io::Result<std::process::Output> {
                unreachable!()
            }
            fn status(&self, _: &str, _: &[&str]) -> std::io::Result<std::process::ExitStatus> {
                unreachable!()
            }
            fn spawn_detached(&self, _: &std::ffi::OsStr, _: &[&str]) -> std::io::Result<()> {
                unreachable!()
            }
            fn output_stdin(
                &self,
                _: &str,
                _: &[&str],
                _: &str,
            ) -> std::io::Result<std::process::Output> {
                unreachable!()
            }
        }
        let outcome = fixture
            .perform(Arc::new(Runner {
                herdr: herdr.to_str().unwrap().into(),
            }))
            .unwrap();
        assert_eq!(outcome.status, Status::Updated);
        assert!(fixture.root.join("bin/picker.sh").exists());
        assert!(state::try_lock_private(&fixture.dir.join("state/install"))
            .unwrap()
            .is_some());
    }
}
