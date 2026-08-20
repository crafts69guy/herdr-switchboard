//! Codex and Claude quota adapters behind the feature-private provider interface.

mod claude;
mod codex;
mod common;

use anyhow::Result;

use super::Report;
use crate::config::Config;
use crate::runner::CommandRunner;
pub(super) use claude::Claude;
use codex::Codex;

#[cfg(test)]
pub(super) use claude::*;
#[cfg(test)]
pub(super) use codex::*;
#[cfg(test)]
pub(super) use common::*;

/// One source of quota numbers. Adding a provider is an impl plus a line in
/// [`providers`] — the same shape as the entry sources in [`crate::source`].
///
/// `offline` splits the registry in two: an offline provider is read on the
/// main thread before the terminal is claimed (it costs a file read), while the
/// rest are fetched on a worker so the first frame never waits on a socket.
pub(super) trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn offline(&self) -> bool;
    fn load(&self, runner: &dyn CommandRunner, cfg: &Config) -> Result<Report>;
}

pub(super) fn providers() -> Vec<Box<dyn Provider>> {
    vec![Box::new(Codex), Box::new(Claude)]
}
