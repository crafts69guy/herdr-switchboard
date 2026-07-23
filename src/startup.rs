//! First-paint startup state: claim the terminal immediately, animate a small
//! cat, and let the source commands run on a worker instead of leaving a blank
//! herdr overlay while `git worktree` walks every repository.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::data::{self, Config, Entry, Kind, Theme};
use crate::runner::SystemRunner;
use crate::source;

/// Per-frame delay for the ASCII cat animation, chosen so the cat visibly
/// advances through three frames during the minimum splash window below.
const FRAME_TIME: Duration = Duration::from_millis(140);
/// Long enough for the 140ms cat to visibly advance through three frames. The
/// clock begins at the first attempted splash draw, not when the worker spawns,
/// so terminal setup can never consume the animation before it is visible.
const MIN_VISIBLE: Duration = Duration::from_millis(420);

enum Message {
    Progress(&'static str),
    Ready(Vec<Entry>),
}

pub enum Poll {
    Pending,
    Changed,
    Ready(Vec<Entry>),
    Failed,
}

pub struct State {
    rx: Receiver<Message>,
    since: Instant,
    shown_at: Option<Instant>,
    ready: Option<Vec<Entry>>,
    pub status: String,
}

impl State {
    pub fn spawn(cfg: Config, theme: Theme) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let runner = SystemRunner;
            let _ = tx.send(Message::Progress("Finding the ghq root"));
            let root = data::ghq_root(&runner);
            let ctx = source::LoadCtx {
                runner: &runner,
                theme: &theme,
                root: &root,
            };
            let entries = source::load_all_reporting(&cfg, &ctx, |kind| {
                let status = match kind {
                    Kind::Agent => "Finding running agents",
                    Kind::Workspace => "Reading open workspaces",
                    Kind::Repo => "Indexing repositories",
                    Kind::Worktree => "Checking linked worktrees",
                };
                let _ = tx.send(Message::Progress(status));
            });
            let _ = tx.send(Message::Ready(entries));
        });
        State {
            rx,
            since: Instant::now(),
            shown_at: None,
            ready: None,
            status: "Waking up".into(),
        }
    }

    /// A splash with no data worker behind it. The review pre-roll
    /// (`main::review_splash`) animates the same cat while it warms the diff's
    /// cache, so `poll` is never called and the channel stays disconnected —
    /// `frame()`/`status` and `begin_display` are all it drives.
    pub fn animation(status: &str) -> Self {
        let (_tx, rx) = mpsc::channel();
        State {
            rx,
            since: Instant::now(),
            shown_at: None,
            ready: None,
            status: status.into(),
        }
    }

    #[cfg(test)]
    pub fn waiting(status: &str) -> Self {
        Self::animation(status)
    }

    #[cfg(test)]
    pub fn ready(entries: Vec<Entry>) -> Self {
        let (tx, rx) = mpsc::channel();
        tx.send(Message::Ready(entries))
            .expect("test receiver lives");
        State {
            rx,
            since: Instant::now(),
            // This helper tests result installation, not the visibility hold.
            shown_at: Instant::now().checked_sub(MIN_VISIBLE),
            ready: None,
            status: "Ready".into(),
        }
    }

    /// Start both the animation and its minimum visibility window. Calling this
    /// on later redraws is a no-op, so a resize cannot extend startup.
    pub fn begin_display(&mut self) {
        self.shown_at.get_or_insert_with(Instant::now);
    }

    pub fn frame(&self) -> usize {
        let since = self.shown_at.unwrap_or(self.since);
        (since.elapsed().as_millis() / FRAME_TIME.as_millis()) as usize
    }

    pub fn poll(&mut self) -> Poll {
        if self.ready.is_some() && self.minimum_visible() {
            return Poll::Ready(self.ready.take().unwrap_or_default());
        }
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(Message::Progress(status)) => {
                    self.status = status.into();
                    changed = true;
                }
                Ok(Message::Ready(entries)) => {
                    self.status = "Ready".into();
                    self.ready = Some(entries);
                    if self.minimum_visible() {
                        return Poll::Ready(self.ready.take().unwrap_or_default());
                    }
                    return Poll::Changed;
                }
                Err(TryRecvError::Disconnected) if self.ready.is_some() => return Poll::Pending,
                Err(TryRecvError::Disconnected) => return Poll::Failed,
                Err(TryRecvError::Empty) => {
                    return if changed {
                        Poll::Changed
                    } else {
                        Poll::Pending
                    };
                }
            }
        }
    }

    fn minimum_visible(&self) -> bool {
        self.shown_at
            .is_some_and(|shown_at| shown_at.elapsed() >= MIN_VISIBLE)
    }
}

pub fn draw(f: &mut Frame, area: Rect, theme: &Theme, title: Color, state: &State) {
    let glow = theme.or("green", Color::Green);
    let text = theme.or("text", Color::Reset);
    let sub = theme.or("subtext0", Color::DarkGray);

    let frame = state.frame();
    let dots = ".".repeat(frame % 4);
    let status = format!("{}{}", state.status, dots);
    let compact = area.width < 36 || area.height < 12;
    let mut lines = Vec::new();

    if compact {
        lines.push(Line::styled(
            r" /\_/\",
            Style::default().fg(text).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(vec![
            Span::styled("( ", Style::default().fg(text)),
            Span::styled(
                if frame % 12 == 7 { "o.-" } else { "o.o" },
                Style::default().fg(glow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" )", Style::default().fg(text)),
        ]));
        lines.push(Line::styled(" > ^ <", Style::default().fg(text)));
    } else {
        let (left_paw, right_paw) = if frame % 4 < 2 {
            (" _|", "|_ ")
        } else {
            (" /|", "|\\ ")
        };
        let butterfly = if frame % 10 < 5 { "*" } else { "+" };
        lines.push(Line::from(vec![
            Span::styled(format!("{butterfly}   "), Style::default().fg(glow)),
            Span::styled(
                r"/\_____/\",
                Style::default().fg(text).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::styled(
            r"   /         \",
            Style::default().fg(text).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(vec![
            Span::styled("  |   ", Style::default().fg(text)),
            Span::styled(
                if frame % 12 == 7 { "o   -" } else { "o   o" },
                Style::default().fg(glow).add_modifier(Modifier::BOLD),
            ),
            Span::styled("   |", Style::default().fg(text)),
        ]));
        lines.push(Line::styled(r"  |     ^     |", Style::default().fg(text)));
        lines.push(Line::styled(r"   \   ---   /", Style::default().fg(text)));
        lines.push(Line::styled(r"    |_______|", Style::default().fg(text)));
        lines.push(Line::from(vec![
            Span::styled(format!("   /{left_paw}"), Style::default().fg(text)),
            Span::styled("  tap tap  ", Style::default().fg(sub)),
            Span::styled(format!("{right_paw}\\"), Style::default().fg(text)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" .--", Style::default().fg(text)),
            Span::styled(
                "[=]",
                Style::default().fg(glow).add_modifier(Modifier::BOLD),
            ),
            Span::styled("--[ ][ ][ ]--", Style::default().fg(text)),
            Span::styled(
                "[=]",
                Style::default().fg(glow).add_modifier(Modifier::BOLD),
            ),
            Span::styled("--.", Style::default().fg(text)),
        ]));
        lines.push(Line::styled(
            " '-----------------------'",
            Style::default().fg(title),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        status,
        Style::default().fg(text).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::styled(
        "Esc or Ctrl-C to cancel",
        Style::default().fg(sub),
    ));

    let content_height = lines.len() as u16;
    let top = area
        .y
        .saturating_add(area.height.saturating_sub(content_height) / 2);
    let draw_area = Rect::new(area.x, top, area.width, content_height.min(area.height));
    f.render_widget(
        Paragraph::new(Text::from(lines)).alignment(Alignment::Center),
        draw_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_worker_waits_for_first_draw_and_minimum_visibility() {
        let (tx, rx) = mpsc::channel();
        tx.send(Message::Ready(Vec::new()))
            .expect("test receiver lives");
        drop(tx);
        let mut state = State {
            rx,
            since: Instant::now(),
            shown_at: None,
            ready: None,
            status: "Waking up".into(),
        };

        assert!(matches!(state.poll(), Poll::Changed));
        assert!(state.ready.is_some());
        // A ready result may not bypass the first frame.
        assert!(matches!(state.poll(), Poll::Pending));

        state.begin_display();
        assert_eq!(state.frame(), 0);
        assert!(matches!(state.poll(), Poll::Pending));

        state.shown_at = Instant::now().checked_sub(MIN_VISIBLE);
        assert!(matches!(state.poll(), Poll::Ready(entries) if entries.is_empty()));
    }
}
