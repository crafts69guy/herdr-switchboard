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

        let transition = if event::poll(surface.tick_rate())? {
            surface.on_event(event::read()?)?
        } else {
            surface.on_tick()?
        };

        match transition {
            Transition::Wait => {}
            Transition::Redraw => dirty = true,
            Transition::Exit(output) => return Ok(output),
        }
    }
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
    use super::*;

    #[test]
    fn transition_output_is_typed() {
        let transition: Transition<&str> = Transition::Exit("chosen");
        assert!(matches!(transition, Transition::Exit("chosen")));
    }
}
