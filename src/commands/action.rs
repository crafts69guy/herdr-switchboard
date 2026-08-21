//! Terminal delivery, multiline confirmation, and clipboard effects.

use std::io::Write;
use std::process::Command;

use anyhow::Result;

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
