//! Terminal delivery, multiline confirmation, and clipboard effects.

use std::env;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

pub(super) fn send_to_pane(pane: &str, text: &str, run: bool) -> Result<()> {
    anyhow::ensure!(!pane.is_empty(), "origin pane is unavailable");
    let verb = if run { "run" } else { "send-text" };
    let status = Command::new("herdr")
        .args(["pane", verb, pane, text])
        .status()?;
    anyhow::ensure!(status.success(), "herdr pane {verb} failed");
    Ok(())
}

pub(super) fn confirm_multiline(command: &str) -> Result<()> {
    if !command.contains('\n') {
        return Ok(());
    }
    println!("\x1b[1mRun multiline command?\x1b[0m\n\n{command}\n");
    print!("Type run to confirm: ");
    std::io::stdout().flush()?;
    let mut reply = String::new();
    std::io::stdin().read_line(&mut reply)?;
    anyhow::ensure!(reply.trim() == "run", "multiline run cancelled");
    Ok(())
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn copy_text(text: &str) -> Result<()> {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if program_exists("wl-copy") {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {program}"))?;
    child
        .stdin
        .as_mut()
        .context("clipboard stdin unavailable")?
        .write_all(text.as_bytes())?;
    anyhow::ensure!(child.wait()?.success(), "clipboard command failed");
    Ok(())
}

fn program_exists(program: &str) -> bool {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .any(|directory| directory.join(program).is_file())
}
