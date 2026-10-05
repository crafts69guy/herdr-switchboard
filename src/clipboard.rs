//! Cross-platform clipboard delivery for text selected by Switchboard surfaces.

use std::env;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

pub(crate) fn copy_text(text: &str) -> Result<()> {
    let (program, args) = clipboard_command(cfg!(target_os = "macos"), program_exists("wl-copy"));
    deliver(program, args, text)
}

/// Which program owns the clipboard: pbcopy on macOS, wl-copy under Wayland,
/// and xclip otherwise.
fn clipboard_command(macos: bool, wayland: bool) -> (&'static str, &'static [&'static str]) {
    if macos {
        ("pbcopy", &[])
    } else if wayland {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    }
}

/// Hand `text` to `program` on stdin and require a clean exit.
fn deliver(program: &str, args: &[&str], text: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_reaches_its_own_clipboard_program() {
        assert_eq!(clipboard_command(true, true).0, "pbcopy");
        assert_eq!(clipboard_command(false, true).0, "wl-copy");
        assert_eq!(
            clipboard_command(false, false),
            ("xclip", &["-selection", "clipboard"][..])
        );
    }

    /// The real spawn path, against `sh` standing in for the clipboard: the
    /// text must arrive on stdin, and a failing or missing program is an error.
    #[test]
    fn text_reaches_the_program_on_stdin_and_failures_are_reported() {
        deliver("sh", &["-c", r#"test "$(cat)" = "copied""#], "copied").expect("delivered");
        assert!(deliver("sh", &["-c", "cat >/dev/null; exit 1"], "x").is_err());
        assert!(deliver("switchboard-no-such-clipboard", &[], "x").is_err());
    }

    #[test]
    fn a_program_is_found_only_on_path() {
        assert!(program_exists("sh"));
        assert!(!program_exists("switchboard-no-such-clipboard"));
    }
}
