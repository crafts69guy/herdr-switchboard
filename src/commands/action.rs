//! Terminal delivery, multiline confirmation, and clipboard effects.

use std::io::{BufRead, Write};

use anyhow::Result;

use crate::runner::CommandRunner;

pub(super) fn send_to_pane(
    runner: &dyn CommandRunner,
    pane: &str,
    text: &str,
    run: bool,
) -> Result<()> {
    anyhow::ensure!(!pane.is_empty(), "origin pane is unavailable");
    let verb = if run { "run" } else { "send-text" };
    let status = runner.status("herdr", &["pane", verb, pane, text])?;
    anyhow::ensure!(status.success(), "herdr pane {verb} failed");
    Ok(())
}

/// Ask on the restored terminal before running a command that spans lines.
pub(super) fn confirm_multiline(command: &str) -> Result<()> {
    confirm_multiline_with(
        command,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout(),
    )
}

/// [`confirm_multiline`] over any reader and writer, which is what makes the
/// prompt testable without a terminal.
pub(super) fn confirm_multiline_with(
    command: &str,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<()> {
    if !command.contains('\n') {
        return Ok(());
    }
    write!(
        output,
        "\x1b[1mRun multiline command?\x1b[0m\n\n{command}\n\nType run to confirm: "
    )?;
    output.flush()?;
    let mut reply = String::new();
    input.read_line(&mut reply)?;
    anyhow::ensure!(reply.trim() == "run", "multiline run cancelled");
    Ok(())
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    #[test]
    fn fill_sends_text_and_run_runs_it_in_the_origin_pane() {
        let runner = MockRunner::new();
        send_to_pane(&runner, "w1:p1", "git status", false).expect("fills");
        send_to_pane(&runner, "w1:p1", "git status", true).expect("runs");
        assert_eq!(
            runner.calls(),
            vec![
                vec!["herdr", "pane", "send-text", "w1:p1", "git status"],
                vec!["herdr", "pane", "run", "w1:p1", "git status"],
            ]
        );
    }

    #[test]
    fn delivery_refuses_a_missing_pane_and_reports_a_failed_herdr() {
        let runner = MockRunner::new();
        assert!(send_to_pane(&runner, "", "ls", true).is_err());
        assert!(runner.calls().is_empty(), "nothing is sent without a pane");

        let failing = MockRunner::new().failing("herdr");
        let error = send_to_pane(&failing, "w1:p1", "ls", true).unwrap_err();
        assert!(error.to_string().contains("pane run"), "{error}");
    }

    #[test]
    fn a_single_line_command_needs_no_confirmation() {
        let mut output = Vec::new();
        confirm_multiline_with("ls", &mut "".as_bytes(), &mut output).expect("no prompt");
        assert!(output.is_empty());
    }

    #[test]
    fn a_multiline_command_runs_only_after_typing_run() {
        let mut output = Vec::new();
        confirm_multiline_with("a\nb", &mut "run\n".as_bytes(), &mut output).expect("confirmed");
        let shown = String::from_utf8(output).expect("utf-8");
        assert!(
            shown.contains("a\nb"),
            "the whole command is shown: {shown}"
        );

        let mut output = Vec::new();
        let refused = confirm_multiline_with("a\nb", &mut "yes\n".as_bytes(), &mut output);
        assert!(refused.is_err(), "anything but `run` cancels");
    }

    #[test]
    fn a_quoted_path_survives_single_quotes() {
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
    }

    struct BrokenPipe;
    impl std::io::Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A terminal that cannot be written to cancels rather than runs.
    #[test]
    fn an_unwritable_terminal_cancels_the_prompt() {
        let mut output = BrokenPipe;
        assert!(confirm_multiline_with("a\nb", &mut "run\n".as_bytes(), &mut output).is_err());
        assert!(output.flush().is_ok());
    }
}
