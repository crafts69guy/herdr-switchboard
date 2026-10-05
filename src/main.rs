//! herdr-switchboard — argv composition root for every Switchboard mode.

mod action;
mod agent_handoff;
mod agents;
mod changelog;
mod chrome;
mod clipboard;
mod commands;
mod config;
mod data;
mod fnm;
mod fnm_manager;
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
    let open = OpenRequest::parse(args);
    let cfg = Config::try_load()?;
    action::open_target(
        &runner::SystemRunner,
        &open.target,
        &open.path,
        &open.origin,
        &open.label,
        &cfg,
    )
}

/// The flags `open` understands; anything else, and a flag missing its value,
/// is ignored rather than fatal, because Bash builds this line.
#[derive(Debug, Default, PartialEq, Eq)]
struct OpenRequest {
    target: String,
    path: String,
    origin: String,
    label: String,
}

impl OpenRequest {
    fn parse(args: &[String]) -> Self {
        let mut open = Self::default();
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let val = it.next().cloned().unwrap_or_default();
            match flag.as_str() {
                "--target" => open.target = val,
                "--path" => open.path = val,
                "--origin" => open.origin = val,
                "--label" => open.label = val,
                _ => {}
            }
        }
        open
    }
}

/// `herdr-switchboard config get KEY [DEFAULT]` — the scalar config reader,
/// so bash reads a setting through the same parser the TUI uses.
fn cli_config(args: &[String]) -> Result<()> {
    println!("{}", config_value(&Config::try_load()?, args)?);
    Ok(())
}

fn config_value(cfg: &Config, args: &[String]) -> Result<String> {
    match args.first().map(String::as_str) {
        Some("get") => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            let default = args.get(2).map(String::as_str).unwrap_or("");
            Ok(cfg.value_for_cli(key).unwrap_or_else(|| default.into()))
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
    run(&collect_startup_args(env::args_os())?)
}

/// Every mode, chosen by its first argument; no argument is the switcher.
fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("{}", version_line());
            Ok(())
        }
        Some("--changelog") => changelog::main(),
        Some("--update-check") => update::main(),
        Some("--git") => git::main(),
        Some("--fnm") => fnm_manager::main(Config::try_load()?, Theme::load()),
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

/// What `--version` prints; `bin/lib.sh` matches the binary against the manifest
/// with it, so its shape is a contract.
fn version_line() -> String {
    format!("herdr-switchboard {}", env!("CARGO_PKG_VERSION"))
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

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn open_reads_its_flags_in_any_order_and_ignores_the_rest() {
        let open = OpenRequest::parse(&strings(&[
            "--label",
            "repo",
            "--bogus",
            "x",
            "--path",
            "/src/repo",
            "--target",
            "tab",
            "--origin",
            "w1:p1",
        ]));
        assert_eq!(
            open,
            OpenRequest {
                target: "tab".into(),
                path: "/src/repo".into(),
                origin: "w1:p1".into(),
                label: "repo".into(),
            }
        );
        // A trailing flag with no value reads as empty rather than failing.
        assert_eq!(OpenRequest::parse(&strings(&["--path"])).path, "");
    }

    #[test]
    fn config_get_answers_the_typed_value_or_the_callers_default() {
        let mut cfg = Config::default();
        cfg.projects.default_target = "workspace".into();
        let value = config_value(&cfg, &strings(&["get", "default_target"])).unwrap();
        assert_eq!(value, "workspace");
        let fallback = config_value(&cfg, &strings(&["get", "no.such.key", "dflt"])).unwrap();
        assert_eq!(fallback, "dflt");
        assert!(config_value(&cfg, &strings(&["set", "x"])).is_err());
        assert!(config_value(&cfg, &[]).is_err());
    }

    #[test]
    fn version_names_the_binary_and_its_manifest_version() {
        assert_eq!(
            version_line(),
            format!("herdr-switchboard {}", env!("CARGO_PKG_VERSION"))
        );
        run(&strings(&["--version"])).expect("prints and exits cleanly");
    }
}
