//! Cross-platform clipboard delivery for text selected by Switchboard surfaces.

use std::env;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

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
