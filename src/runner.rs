//! The one seam between this plugin and the programs it drives.
//!
//! Every `herdr` / `ghq` / `git` / `rm` call in the data, action, and preview
//! layers goes through a [`CommandRunner`] rather than [`std::process::Command`]
//! directly. In production that is [`SystemRunner`], a thin wrapper; in tests it
//! is [`MockRunner`], which returns canned output and records the argv it was
//! handed. That is what lets the JSON→entry mapping, the herdr verb building,
//! and the JSON→card rendering be unit-tested at all — none of them shells out
//! for real.
//!
//! The interactive-but-detached fetch in [`crate::update`] is deliberately *not*
//! routed here: it needs its own process group and null stdio, its value is the
//! tag parsing (already tested), and mocking `git ls-remote` would test nothing
//! the parser test does not.

use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Output, Stdio};

/// Runs external commands. [`output`](Self::output) captures stdout for parsing,
/// [`status`](Self::status) inherits the terminal, and
/// [`spawn_detached`](Self::spawn_detached) starts a background worker.
pub trait CommandRunner {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output>;
    fn status(&self, program: &str, args: &[&str]) -> io::Result<ExitStatus>;
    fn spawn_detached(&self, program: &OsStr, args: &[&str]) -> io::Result<()>;

    /// Trimmed stdout when the command exits 0; `None` on spawn failure or a
    /// non-zero exit. The common read path.
    fn capture(&self, program: &str, args: &[&str]) -> Option<String> {
        let out = self.output(program, args).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// True when the command ran and exited 0. The common "did it work" path.
    fn ok(&self, program: &str, args: &[&str]) -> bool {
        self.status(program, args)
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// The production runner: spawn the real program.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        Command::new(program).args(args).output()
    }

    fn status(&self, program: &str, args: &[&str]) -> io::Result<ExitStatus> {
        Command::new(program).args(args).status()
    }

    fn spawn_detached(&self, program: &OsStr, args: &[&str]) -> io::Result<()> {
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map(|_| ())
    }
}

#[cfg(test)]
pub use mock::MockRunner;

#[cfg(test)]
mod mock {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::io;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    use super::CommandRunner;

    /// A runner that answers from a seeded table and records every argv, so a
    /// test can assert both what a card did with canned JSON and what verbs a
    /// dispatch built. Matching is by substring of the `"program arg1 arg2 …"`
    /// join, so a test seeds `"agent get term-1"` without spelling every flag.
    #[derive(Default)]
    pub struct MockRunner {
        responses: Vec<(String, String)>,
        failures: Vec<String>,
        pub calls: RefCell<Vec<Vec<String>>>,
    }

    impl MockRunner {
        pub fn new() -> Self {
            Self::default()
        }

        /// Seed stdout for any command whose joined argv contains `needle`.
        pub fn on(mut self, needle: &str, stdout: &str) -> Self {
            self.responses
                .push((needle.to_string(), stdout.to_string()));
            self
        }

        /// Make any command whose joined argv contains `needle` exit non-zero.
        pub fn failing(mut self, needle: &str) -> Self {
            self.failures.push(needle.to_string());
            self
        }

        /// Every argv this runner was handed, program first, in call order.
        pub fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }

        fn record(&self, program: &str, args: &[&str]) -> String {
            let mut argv = vec![program.to_string()];
            argv.extend(args.iter().map(|a| a.to_string()));
            let joined = argv.join(" ");
            self.calls.borrow_mut().push(argv);
            joined
        }

        fn succeeds(&self, joined: &str) -> bool {
            !self.failures.iter().any(|n| joined.contains(n))
        }

        fn body(&self, joined: &str) -> Vec<u8> {
            self.responses
                .iter()
                .find(|(needle, _)| joined.contains(needle.as_str()))
                .map(|(_, out)| out.as_bytes().to_vec())
                .unwrap_or_default()
        }
    }

    fn exit(success: bool) -> ExitStatus {
        // Unix wait-status: exit code n is n << 8; this plugin is unix-only.
        ExitStatus::from_raw(if success { 0 } else { 1 << 8 })
    }

    impl CommandRunner for MockRunner {
        fn output(&self, program: &str, args: &[&str]) -> io::Result<Output> {
            let joined = self.record(program, args);
            let success = self.succeeds(&joined);
            Ok(Output {
                status: exit(success),
                stdout: if success {
                    self.body(&joined)
                } else {
                    Vec::new()
                },
                stderr: Vec::new(),
            })
        }

        fn status(&self, program: &str, args: &[&str]) -> io::Result<ExitStatus> {
            let joined = self.record(program, args);
            Ok(exit(self.succeeds(&joined)))
        }

        fn spawn_detached(&self, program: &OsStr, args: &[&str]) -> io::Result<()> {
            let joined = self.record(&program.to_string_lossy(), args);
            if self.succeeds(&joined) {
                Ok(())
            } else {
                Err(io::Error::other("mock detached spawn failed"))
            }
        }
    }
}
