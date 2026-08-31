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
pub fn run<S: Surface>(surface: &mut S) -> Result<S::Output> {
    let mut terminal = claim_terminal();
    let _restore = RestoreGuard;
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
        let outcome = drain_frame(
            |blocking| {
                let wait = if blocking { tick } else { Duration::ZERO };
                if event::poll(wait)? {
                    surface.on_event(event::read()?)
                } else {
                    surface.on_tick()
                }
            },
            // A zero timeout answers from the queue without waiting, so the
            // burst only continues while input is genuinely already pending.
            || Ok(event::poll(Duration::ZERO)?),
            &mut dirty,
        )?;
        if let Some(output) = outcome {
            return Ok(output);
        }
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
}
