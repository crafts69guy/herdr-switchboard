//! herdr-switchboard — argv composition root for every Switchboard mode.

mod action;
mod agents;
mod changelog;
mod chrome;
mod commands;
mod config;
mod data;
mod git;
mod history;
mod keymap;
mod markdown;
mod menu;
mod notify;
mod picker;
mod ports;
mod projects;
mod query;
mod runner;
mod settings;
mod socket;
mod source;
mod splash;
mod state;
mod surface;
mod trace;
mod tui;
mod update;
mod usage;
mod zen;

use std::env;
use std::ffi::OsString;
use std::fmt;

use anyhow::Result;

use data::{Config, Theme};

/// `herdr-switchboard open --target T --path P --origin O --label L` — the
/// clone flow (`bin/get.sh`) delegates here so the herdr open verbs live only in
/// Rust rather than being mirrored in bash.
fn cli_open(args: &[String]) -> Result<()> {
    let (mut target, mut path, mut origin, mut label) =
        (String::new(), String::new(), String::new(), String::new());
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let val = it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--target" => target = val,
            "--path" => path = val,
            "--origin" => origin = val,
            "--label" => label = val,
            _ => {}
        }
    }
    let cfg = Config::try_load()?;
    action::open_target(&runner::SystemRunner, &target, &path, &origin, &label, &cfg)
}

/// `herdr-switchboard config get KEY [DEFAULT]` — the scalar config reader,
/// so bash reads a setting through the same parser the TUI uses.
fn cli_config(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("get") => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            let default = args.get(2).map(String::as_str).unwrap_or("");
            println!(
                "{}",
                Config::try_load()?
                    .value_for_cli(key)
                    .unwrap_or_else(|| default.into())
            );
            Ok(())
        }
        _ => Err(anyhow::anyhow!("usage: config get <key> [default]")),
    }
}

#[derive(Debug)]
struct NonUtf8Argument {
    index: usize,
}

impl fmt::Display for NonUtf8Argument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "command argument {} is not valid UTF-8",
            self.index
        )
    }
}

impl std::error::Error for NonUtf8Argument {}

fn collect_startup_args(argv: impl IntoIterator<Item = OsString>) -> Result<Vec<String>> {
    argv.into_iter()
        .skip(1)
        .enumerate()
        .map(|(offset, argument)| {
            argument
                .into_string()
                .map_err(|_| NonUtf8Argument { index: offset + 1 }.into())
        })
        .collect()
}

fn main() -> Result<()> {
    // First statement on purpose: it fixes the zero point every other trace mark
    // is measured from. Inert unless SWITCHBOARD_TRACE is set.
    trace::init();
    let args = collect_startup_args(env::args_os())?;
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("herdr-switchboard {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--changelog") => changelog::main(),
        Some("--update-check") => update::main(),
        Some("--git") => git::main(),
        Some("--menu") => menu::main(Config::try_load()?, Theme::load()),
        Some("--agent-launch") => agents::launch_worker(&args[1..], &Config::try_load()?),
        Some("--agents") => agents::main(Config::try_load()?, Theme::load()),
        Some("--commands") => commands::main(Config::try_load()?, Theme::load()),
        Some("--ports") => ports::main(Config::try_load()?, Theme::load()),
        Some("--settings") => settings::main(Config::try_load()?, Theme::load()),
        Some("--usage") => usage::main(Config::try_load()?, Theme::load()),
        Some("--zen") => zen::main(Config::try_load()?, Theme::load()),
        Some("open") => cli_open(&args[1..]),
        Some("config") => cli_config(&args[1..]),
        Some("zen") => zen::cli(&runner::SystemRunner, &args[1..], &Config::try_load()?),
        Some("notify") => notify::cli(&args[1..], &Config::try_load()?),
        _ => projects::main(Config::try_load()?, Theme::load()),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    #[test]
    fn startup_args_skip_non_utf8_executable_name_before_conversion() {
        let argv = vec![
            OsString::from_vec(vec![0xff]),
            OsString::from("--agent-launch"),
            OsString::from("--kind"),
            OsString::from("claude"),
        ];

        let args = collect_startup_args(argv).unwrap();

        assert_eq!(args, ["--agent-launch", "--kind", "claude"]);
    }

    #[test]
    fn startup_args_return_typed_error_for_non_utf8_command_argument() {
        let argv = vec![
            OsString::from("herdr-switchboard"),
            OsString::from("--title"),
            OsString::from_vec(vec![0xff]),
        ];

        let error = collect_startup_args(argv).unwrap_err();

        assert_eq!(error.downcast_ref::<NonUtf8Argument>().unwrap().index, 2);
    }
}
