//! Durable, session-scoped tasks. Herdr lifecycle is observation, never task completion.

mod cli;
mod effect;
mod model;
mod store;
#[cfg(test)]
mod tests;
mod view;

pub(crate) use cli::entry;
pub(crate) use view::main;

use anyhow::Result;
use std::fmt;

#[derive(Debug)]
struct Fault {
    code: &'static str,
    message: &'static str,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for Fault {}

fn require(condition: bool, code: &'static str, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Fault { code, message }.into())
    }
}
