//! Universal host for every Switchboard terminal surface.
//!
//! A surface owns its model and translates input into state transitions. The
//! host owns the terminal lease, event scheduling, redraw policy, and teardown.
//! Domain work stays outside this module: a surface may return a typed output,
//! after which its caller runs the corresponding effect with the terminal
//! already restored.

use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use ratatui::Frame;

const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1000l";

/// How many already-queued events one frame may absorb before it must repaint.
///
/// The cap is what keeps coalescing from becoming starvation: a bracketed paste
/// or a key held down long enough arrives faster than the loop retires it, and
/// without a ceiling the surface would keep swallowing input while the screen
/// stood still. Far above any wheel or autorepeat burst, far below the size of a
/// paste.
const MAX_COALESCED_EVENTS: usize = 64;

fn claim_terminal() -> ratatui::DefaultTerminal {
    let terminal = ratatui::init();
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        print!("{MOUSE_OFF}");
        let _ = io::stdout().flush();
        restore(info);
    }));
    print!("{MOUSE_ON}");
    let _ = io::stdout().flush();
    terminal
}

pub(crate) fn restore_terminal() {
    print!("{MOUSE_OFF}");
    let _ = io::stdout().flush();
    ratatui::restore();
}

/// What the host should do after a surface observes input or a timer tick.
pub enum Transition<O> {
    /// Keep waiting without repainting.
    Wait,
    /// Repaint before waiting again.
    Redraw,
    /// Restore the terminal and return this output to the caller.
    Exit(O),
}

/// One terminal surface hosted by [`run`].
///
/// `on_event` and `on_tick` must be non-blocking. External or interactive work
/// is represented by `Output` and performed by the caller after [`run`]
/// returns, which makes restore-before-effect an invariant of the interface.
pub trait Surface {
    type Output;

    fn draw(&mut self, frame: &mut Frame);

    fn on_event(&mut self, event: Event) -> Result<Transition<Self::Output>>;

    fn on_tick(&mut self) -> Result<Transition<Self::Output>> {
        Ok(Transition::Wait)
    }

    fn tick_rate(&self) -> Duration {
        Duration::from_millis(200)
    }

    fn terminal_claimed(&mut self) {}

    /// Called after geometry has been published by `draw`. Deferred work such
    /// as a width-aware preview request starts here, never before the frame.
    fn after_draw(&mut self) -> Result<()> {
        Ok(())
    }
}

struct RestoreGuard;

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Run a surface until it returns a typed output.
///
/// The first frame is always drawn immediately. The restore guard is created
/// directly after terminal acquisition, so normal exits and every propagated
/// error disable mouse reporting and restore the screen before returning.
///
/// Input that has already arrived is drained before the next frame. A terminal
/// delivers a wheel turn or a held key as a burst of separate events, and
/// answering each one with its own full repaint means the surface renders the
/// states nobody sees on the way to the one they do — at list sizes where a
/// repaint is the expensive half, that is what a scroll stutter is made of.
/// Every event still reaches `on_event` in order, and a burst that ends in an
/// `Exit` stops there rather than being drawn.
pub(crate) fn run<S: Surface>(surface: &mut S) -> Result<S::Output> {
    let mut terminal = claim_terminal();
    let _restore = RestoreGuard;
    host(surface, &mut terminal, &mut TerminalInput)
}

/// Something that can host a surface to completion. Production uses
/// [`TerminalHost`]; a mode's entry point takes `&mut impl Host` so a test can
/// drive the same code path on a `TestBackend` with scripted input.
pub(crate) trait Host {
    fn run<S: Surface>(&mut self, surface: &mut S) -> Result<S::Output>;
}

/// The real terminal: claim it, host the surface, restore it.
pub(crate) struct TerminalHost;

impl Host for TerminalHost {
    fn run<S: Surface>(&mut self, surface: &mut S) -> Result<S::Output> {
        run(surface)
    }
}

/// A host for tests: a `TestBackend` of the given size and a script of
/// events. [`ScriptedHost::PAUSE`] in the script lets the surface tick for a
/// while, so background work can land before the next key. Once the script
/// runs out the surface may tick a bounded number of times and then the host
/// fails, so a surface that never exits cannot hang the suite.
#[cfg(test)]
pub(crate) struct ScriptedHost {
    pub(crate) events: std::collections::VecDeque<Event>,
    pub(crate) size: (u16, u16),
}

#[cfg(test)]
impl ScriptedHost {
    pub(crate) fn new(events: impl IntoIterator<Item = Event>) -> Self {
        Self {
            events: events.into_iter().collect(),
            size: (120, 40),
        }
    }

    /// A key press for a script.
    pub(crate) fn key(code: event::KeyCode, modifiers: event::KeyModifiers) -> Event {
        Event::Key(event::KeyEvent::new(code, modifiers))
    }

    /// Not an input: a point in the script where the surface ticks instead.
    pub(crate) const PAUSE: Event = Event::FocusLost;
}

#[cfg(test)]
impl Host for ScriptedHost {
    fn run<S: Surface>(&mut self, surface: &mut S) -> Result<S::Output> {
        struct Script<'a> {
            events: &'a mut std::collections::VecDeque<Event>,
            idle: usize,
            pause: usize,
        }
        impl Input for Script<'_> {
            fn poll(&mut self, _wait: Duration) -> Result<bool> {
                if self.pause > 0 {
                    self.pause -= 1;
                    std::thread::sleep(Duration::from_millis(2));
                    return Ok(false);
                }
                if self.events.front() == Some(&ScriptedHost::PAUSE) {
                    self.events.pop_front();
                    self.pause = 100;
                    return Ok(false);
                }
                if !self.events.is_empty() {
                    return Ok(true);
                }
                self.idle += 1;
                anyhow::ensure!(
                    self.idle < 400,
                    "script exhausted before the surface exited"
                );
                std::thread::sleep(Duration::from_millis(2));
                Ok(false)
            }
            fn read(&mut self) -> Result<Event> {
                self.events
                    .pop_front()
                    .ok_or_else(|| anyhow::anyhow!("read past the script"))
            }
        }
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(self.size.0, self.size.1))?;
        host(
            surface,
            &mut terminal,
            &mut Script {
                events: &mut self.events,
                idle: 0,
                pause: 0,
            },
        )
    }
}

/// Every key a surface could meet — each base key under each modifier — and
/// every mouse kind over a grid of `width`×`height`, for sweeps that check a
/// surface answers all of them without panicking. `skip` drops chords a
/// sweep must not press, such as one that would write the user's state.
#[cfg(test)]
pub(crate) fn every_event(
    width: u16,
    height: u16,
    skip: &[(event::KeyCode, event::KeyModifiers)],
) -> Vec<Event> {
    use event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut codes: Vec<KeyCode> = ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .chain([' ', '/', '?', ',', '.', '-', '!'])
        .map(KeyCode::Char)
        .collect();
    codes.extend([
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Backspace,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Delete,
        KeyCode::F(1),
    ]);
    let mut events = Vec::new();
    for modifiers in [
        KeyModifiers::NONE,
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SHIFT,
    ] {
        for code in &codes {
            if !skip.contains(&(*code, modifiers)) {
                events.push(Event::Key(event::KeyEvent::new(*code, modifiers)));
            }
        }
    }
    for row in (0..height).step_by(2) {
        for column in (0..width).step_by(4) {
            for kind in [
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollUp,
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::Moved,
            ] {
                events.push(Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                }));
            }
        }
    }
    events.push(Event::Resize(width, height));
    events.push(Event::FocusGained);
    events
}

/// Where the host's events come from: the real terminal in production, a
/// script in tests.
trait Input {
    /// Whether an event is ready within `wait`.
    fn poll(&mut self, wait: Duration) -> Result<bool>;
    fn read(&mut self) -> Result<Event>;
}

struct TerminalInput;

impl Input for TerminalInput {
    fn poll(&mut self, wait: Duration) -> Result<bool> {
        Ok(event::poll(wait)?)
    }
    fn read(&mut self) -> Result<Event> {
        Ok(event::read()?)
    }
}

/// The host loop over any backend and input source. [`run`] hands it the
/// claimed terminal; a test hands it a `TestBackend` and a scripted input.
fn host<S: Surface, B: ratatui::backend::Backend>(
    surface: &mut S,
    terminal: &mut ratatui::Terminal<B>,
    input: &mut impl Input,
) -> Result<S::Output> {
    surface.terminal_claimed();
    let mut dirty = true;

    loop {
        if dirty {
            terminal.draw(|frame| surface.draw(frame))?;
            surface.after_draw()?;
            dirty = false;
        }

        // Block for the first event; take any already queued behind it for free.
        let tick = surface.tick_rate();
        let input = &mut *input;
        let pending = std::cell::RefCell::new(input);
        let outcome = drain_frame(
            |blocking| {
                let wait = if blocking { tick } else { Duration::ZERO };
                let mut input = pending.borrow_mut();
                if input.poll(wait)? {
                    let event = input.read()?;
                    let resized = matches!(event, Event::Resize(..));
                    Ok(repaint_after(resized, surface.on_event(event)?))
                } else {
                    surface.on_tick()
                }
            },
            // A zero timeout answers from the queue without waiting, so the
            // burst only continues while input is genuinely already pending.
            || pending.borrow_mut().poll(Duration::ZERO),
            &mut dirty,
        )?;
        if let Some(output) = outcome {
            return Ok(output);
        }
    }
}

/// Force a repaint after a resize, whatever the surface made of the event.
///
/// `Terminal::draw` autoresizes, but it only runs when a frame was asked for,
/// and every surface answers `Event::Resize` from its catch-all `Wait` arm — so
/// shrinking the window left the taller previous frame on screen with its last
/// row, the command bar, outside the pane until the next keypress. Redraw
/// policy is the host's, so no surface has to remember this one.
fn repaint_after<O>(resized: bool, transition: Transition<O>) -> Transition<O> {
    match transition {
        Transition::Wait if resized => Transition::Redraw,
        other => other,
    }
}

/// Feed one frame's worth of input to a surface, and say whether it asked to
/// exit.
///
/// Split out of [`run`] so the coalescing policy is exercised without a terminal
/// behind it: `next` is asked for one transition at a time — blocking only for
/// the first — and `pending` says whether more input is already queued. Order is
/// preserved, an `Exit` ends the burst where it happened, and the cap guarantees
/// the caller gets to repaint.
fn drain_frame<O>(
    mut next: impl FnMut(bool) -> Result<Transition<O>>,
    mut pending: impl FnMut() -> Result<bool>,
    dirty: &mut bool,
) -> Result<Option<O>> {
    let mut blocking = true;
    for _ in 0..MAX_COALESCED_EVENTS {
        match next(blocking)? {
            Transition::Wait => {}
            Transition::Redraw => *dirty = true,
            Transition::Exit(output) => return Ok(Some(output)),
        }
        blocking = false;
        if !pending()? {
            break;
        }
    }
    Ok(None)
}

/// Draw one frame that a replacing TUI can paint over while it prepares.
///
/// The alternate screen deliberately stays claimed on success. Call
/// [`restore_terminal`] only when the subsequent process replacement fails.
pub(crate) fn preroll(draw: impl FnOnce(&mut Frame)) {
    let mut terminal = claim_terminal();
    print!("{MOUSE_OFF}");
    let _ = io::stdout().flush();
    let mut draw = Some(draw);
    let _ = terminal.draw(|frame| {
        if let Some(draw) = draw.take() {
            draw(frame);
        }
    });
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The host, not the surface, notices a resize. Every surface routes
    /// `Event::Resize` through a catch-all `Wait`, so without this the previous
    /// frame stays on a screen that is no longer its size.
    #[test]
    fn a_resize_repaints_even_when_the_surface_ignores_it() {
        assert!(matches!(
            repaint_after(true, Transition::<()>::Wait),
            Transition::Redraw
        ));
        // Anything the surface already decided stands: a resize never turns an
        // exit into another frame, and a redraw is already a redraw.
        assert!(matches!(
            repaint_after(true, Transition::Exit("chosen")),
            Transition::Exit("chosen")
        ));
        assert!(matches!(
            repaint_after(true, Transition::<()>::Redraw),
            Transition::Redraw
        ));
        // Without a resize, a quiet event still costs nothing.
        assert!(matches!(
            repaint_after(false, Transition::<()>::Wait),
            Transition::Wait
        ));
    }

    #[test]
    fn transition_output_is_typed() {
        let transition: Transition<&str> = Transition::Exit("chosen");
        assert!(matches!(transition, Transition::Exit("chosen")));
    }

    /// Drive `drain_frame` over a scripted queue, recording the order in which
    /// transitions were taken and whether the first ask blocked.
    fn drain(script: Vec<Transition<&'static str>>) -> (Option<&'static str>, bool, Vec<bool>) {
        let queue = RefCell::new(
            script
                .into_iter()
                .collect::<std::collections::VecDeque<_>>(),
        );
        let blocked = RefCell::new(Vec::new());
        let mut dirty = false;
        let output = drain_frame(
            |blocking| {
                blocked.borrow_mut().push(blocking);
                Ok(queue.borrow_mut().pop_front().unwrap_or(Transition::Wait))
            },
            || Ok(!queue.borrow().is_empty()),
            &mut dirty,
        )
        .expect("scripted drain cannot fail");
        (output, dirty, blocked.into_inner())
    }

    /// A burst is absorbed into one repaint: every event is taken, in order, and
    /// only the first ask is allowed to block.
    #[test]
    fn a_queued_burst_is_taken_in_one_pass_and_only_the_first_ask_blocks() {
        let (output, dirty, blocked) = drain(vec![
            Transition::Redraw,
            Transition::Wait,
            Transition::Redraw,
            Transition::Redraw,
        ]);

        assert!(output.is_none());
        assert!(dirty, "a burst containing a Redraw must repaint once");
        assert_eq!(blocked, [true, false, false, false]);
    }

    /// An `Exit` ends the burst where it happened. Draining past it would run
    /// the surface's reducer over input that arrived after it decided to leave —
    /// keystrokes typed ahead into what is about to be someone else's terminal.
    #[test]
    fn an_exit_stops_the_burst_and_leaves_the_rest_of_the_queue_alone() {
        let (output, _, blocked) = drain(vec![
            Transition::Redraw,
            Transition::Exit("chosen"),
            Transition::Redraw,
            Transition::Redraw,
        ]);

        assert_eq!(output, Some("chosen"));
        assert_eq!(blocked.len(), 2, "nothing is taken after the Exit");
    }

    /// Input arriving faster than it is retired must still yield a frame. Without
    /// the cap a paste would keep the loop swallowing events while the screen
    /// stood still.
    #[test]
    fn an_endless_queue_still_yields_a_frame() {
        let mut dirty = false;
        let mut taken = 0usize;
        let output = drain_frame::<()>(
            |_| {
                taken += 1;
                Ok(Transition::Redraw)
            },
            || Ok(true), // never drains
            &mut dirty,
        )
        .expect("scripted drain cannot fail");

        assert!(output.is_none());
        assert!(dirty);
        assert_eq!(taken, MAX_COALESCED_EVENTS);
    }

    /// A quiet tick is the same one-shot it always was: `on_tick` answers, the
    /// queue is empty, and the burst ends immediately.
    #[test]
    fn a_quiet_tick_takes_exactly_one_transition() {
        let (output, dirty, blocked) = drain(vec![Transition::Wait]);

        assert!(output.is_none());
        assert!(!dirty);
        assert_eq!(blocked, [true]);
    }

    /// Input from a script: each event is "ready" until the script runs out,
    /// after which every poll times out.
    struct ScriptInput(std::collections::VecDeque<Event>);

    impl Input for ScriptInput {
        fn poll(&mut self, _wait: Duration) -> Result<bool> {
            Ok(!self.0.is_empty())
        }
        fn read(&mut self) -> Result<Event> {
            self.0
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("read past the script"))
        }
    }

    /// A surface that redraws on `r`, exits on `q`, and exits from a tick once
    /// its input has run dry — counting what the host asked of it.
    #[derive(Default)]
    struct Counting {
        claimed: bool,
        draws: usize,
        after_draws: usize,
        events: Vec<String>,
        ticks: usize,
    }

    impl Surface for Counting {
        type Output = &'static str;

        fn draw(&mut self, frame: &mut Frame) {
            self.draws += 1;
            frame.render_widget(ratatui::widgets::Paragraph::new("hosted"), frame.area());
        }

        fn on_event(&mut self, event: Event) -> Result<Transition<Self::Output>> {
            let Event::Key(key) = event else {
                self.events.push("other".into());
                return Ok(Transition::Wait);
            };
            let event::KeyCode::Char(c) = key.code else {
                return Ok(Transition::Wait);
            };
            self.events.push(c.to_string());
            Ok(match c {
                'r' => Transition::Redraw,
                'q' => Transition::Exit("quit"),
                _ => Transition::Wait,
            })
        }

        fn on_tick(&mut self) -> Result<Transition<Self::Output>> {
            self.ticks += 1;
            Ok(Transition::Exit("idle"))
        }

        fn terminal_claimed(&mut self) {
            self.claimed = true;
        }

        fn after_draw(&mut self) -> Result<()> {
            self.after_draws += 1;
            Ok(())
        }
    }

    fn keys(chars: &str) -> ScriptInput {
        ScriptInput(
            chars
                .chars()
                .map(|c| {
                    Event::Key(event::KeyEvent::new(
                        event::KeyCode::Char(c),
                        event::KeyModifiers::NONE,
                    ))
                })
                .collect(),
        )
    }

    fn terminal() -> ratatui::Terminal<ratatui::backend::TestBackend> {
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 3)).unwrap()
    }

    /// The host draws first, then feeds a queued burst in order and repaints
    /// once for it; an exit inside a burst ends it there.
    #[test]
    fn the_host_draws_first_and_coalesces_a_queued_burst() {
        let mut surface = Counting::default();
        let mut terminal = terminal();
        let output = host(&mut surface, &mut terminal, &mut keys("rrrqx")).unwrap();
        assert_eq!(output, "quit");
        assert!(surface.claimed);
        assert_eq!(
            surface.draws, 1,
            "the burst ended in an exit before a repaint"
        );
        assert_eq!(surface.after_draws, 1);
        assert_eq!(surface.events, ["r", "r", "r", "q"]);
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("hosted"));
    }

    /// A redraw is honoured on the next frame, and with no input left the host
    /// falls through to the surface's tick.
    #[test]
    fn the_host_repaints_after_a_redraw_and_ticks_when_input_is_idle() {
        let mut surface = Counting::default();
        let output = host(&mut surface, &mut terminal(), &mut keys("r")).unwrap();
        assert_eq!(output, "idle");
        assert_eq!(surface.draws, 2);
        assert_eq!(surface.ticks, 1);

        // A resize repaints even though the surface itself only waited.
        let mut surface = Counting::default();
        let mut input = ScriptInput([Event::Resize(30, 4)].into_iter().collect());
        host(&mut surface, &mut terminal(), &mut input).unwrap();
        assert_eq!(surface.events, ["other"]);
        assert_eq!(surface.draws, 2);
    }

    /// The default hooks are inert: a surface that only draws and handles
    /// events still ticks, claims, and finishes drawing cleanly.
    #[test]
    fn the_default_surface_hooks_do_nothing() {
        struct Plain;
        impl Surface for Plain {
            type Output = ();
            fn draw(&mut self, _frame: &mut Frame) {}
            fn on_event(&mut self, _event: Event) -> Result<Transition<()>> {
                Ok(Transition::Exit(()))
            }
        }
        let mut plain = Plain;
        assert!(matches!(plain.on_tick().unwrap(), Transition::Wait));
        assert_eq!(plain.tick_rate(), Duration::from_millis(200));
        plain.terminal_claimed();
        plain.after_draw().unwrap();
        host(&mut plain, &mut terminal(), &mut keys("x")).unwrap();
    }
}
