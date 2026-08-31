//! Bounded shell-history ingestion and preset cwd expansion.

use std::env;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;

use super::catalog::Import;

pub(super) fn read_login_shell_history() -> Result<Vec<Import>> {
    let shell = env::var("SHELL").unwrap_or_default();
    let home = env::var("HOME").unwrap_or_default();
    let (kind, path) = if shell.ends_with("/fish") {
        (
            "fish",
            env::var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".local/share"))
                .join("fish/fish_history"),
        )
    } else if shell.ends_with("/bash") {
        (
            "bash",
            env::var("HISTFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".bash_history")),
        )
    } else {
        (
            "zsh",
            env::var("HISTFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".zsh_history")),
        )
    };
    let (text, truncated) = read_tail(&path, 8 * 1024 * 1024)?;
    let text = if truncated {
        trim_to_record_boundary(kind, &text)
    } else {
        text
    };
    Ok(parse_shell_history(kind, &text))
}

fn read_tail(path: &Path, max_bytes: u64) -> Result<(String, bool)> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let truncated = len > max_bytes;
    if truncated {
        file.seek(SeekFrom::Start(len - max_bytes))?;
    }
    let mut bytes = Vec::with_capacity(len.min(max_bytes) as usize);
    file.take(max_bytes).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    if truncated {
        Ok((
            text.split_once('\n')
                .map(|(_, tail)| tail)
                .unwrap_or_default()
                .to_string(),
            true,
        ))
    } else {
        Ok((text.into_owned(), false))
    }
}

pub(super) fn trim_to_record_boundary(kind: &str, text: &str) -> String {
    let is_boundary = |line: &str| match kind {
        "zsh" => line.starts_with(": ") && line.contains(';'),
        "fish" => line.starts_with("- cmd: "),
        "bash" => line
            .strip_prefix('#')
            .is_some_and(|value| value.parse::<u64>().is_ok()),
        _ => false,
    };
    if kind == "bash" && !text.lines().any(is_boundary) {
        return text.to_string();
    }
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if is_boundary(line.trim_end_matches('\n')) {
            return text[offset..].to_string();
        }
        offset += line.len();
    }
    String::new()
}

pub(super) fn resolve_preset_cwd(raw: &str) -> Result<Option<String>> {
    if raw == "origin" || raw.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        !raw.contains("$(") && !raw.contains('`'),
        "preset cwd cannot execute shell syntax"
    );
    let mut expanded = raw.to_string();
    if expanded == "~" || expanded.starts_with("~/") {
        let home = env::var("HOME").context("HOME is unavailable for preset cwd")?;
        expanded = format!("{home}{}", &expanded[1..]);
    }
    static VARIABLE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let variable = VARIABLE.get_or_init(|| {
        Regex::new(r#"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)"#)
            .expect("constant regex")
    });
    expanded = variable
        .replace_all(&expanded, |captures: &regex::Captures<'_>| {
            let name = captures
                .get(1)
                .or_else(|| captures.get(2))
                .map(|value| value.as_str())
                .unwrap_or_default();
            env::var(name).unwrap_or_default()
        })
        .into_owned();
    let path = Path::new(&expanded);
    anyhow::ensure!(
        path.is_absolute(),
        "preset cwd must resolve to an absolute path"
    );
    anyhow::ensure!(path.is_dir(), "preset cwd does not exist");
    Ok(Some(expanded))
}

pub(super) fn parse_shell_history(kind: &str, text: &str) -> Vec<Import> {
    match kind {
        "fish" => parse_fish(text),
        "bash" => parse_bash(text),
        _ => parse_zsh(text),
    }
}

fn parse_zsh(text: &str) -> Vec<Import> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut timestamp = 0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(": ") {
            if !current.is_empty() {
                out.push(Import {
                    command: current,
                    timestamp,
                });
                current = String::new();
            }
            if let Some((meta, command)) = rest.split_once(';') {
                timestamp = meta
                    .split(':')
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                current.push_str(command.trim_end_matches('\\'));
                if command.ends_with('\\') {
                    current.push('\n');
                }
            }
        } else if !current.is_empty() {
            current.push_str(line.trim_end_matches('\\'));
            if line.ends_with('\\') {
                current.push('\n');
            }
        } else if !line.trim().is_empty() {
            out.push(Import {
                command: line.to_string(),
                timestamp: 0,
            });
        }
    }
    if !current.is_empty() {
        out.push(Import {
            command: current,
            timestamp,
        });
    }
    out
}

fn parse_bash(text: &str) -> Vec<Import> {
    let mut out = Vec::new();
    let mut timestamp = 0;
    let mut current = Vec::new();
    for line in text.lines() {
        if let Some(value) = line
            .strip_prefix('#')
            .and_then(|value| value.parse::<u64>().ok())
        {
            if !current.is_empty() {
                out.push(Import {
                    command: current.join("\n"),
                    timestamp,
                });
                current.clear();
            }
            timestamp = value;
        } else if timestamp == 0 {
            if !line.trim().is_empty() {
                out.push(Import {
                    command: line.to_string(),
                    timestamp: 0,
                });
            }
        } else {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        out.push(Import {
            command: current.join("\n"),
            timestamp,
        });
    }
    out
}

fn parse_fish(text: &str) -> Vec<Import> {
    let mut out = Vec::new();
    let mut command: Option<String> = None;
    let mut timestamp = 0;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("- cmd: ") {
            if let Some(command) = command.take() {
                out.push(Import { command, timestamp });
            }
            command = Some(value.replace("\\n", "\n").replace("\\\\", "\\"));
            timestamp = 0;
        } else if let Some(value) = line.trim().strip_prefix("when: ") {
            timestamp = value.parse().unwrap_or(0);
        }
    }
    if let Some(command) = command {
        out.push(Import { command, timestamp });
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn commands(imports: &[Import]) -> Vec<&str> {
        imports.iter().map(|i| i.command.as_str()).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let dir = env::temp_dir().join(format!(
            "switchboard-history-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// zsh writes `: <epoch>:<elapsed>;<command>` and continues a multi-line
    /// command with a trailing backslash. Losing the continuation would offer a
    /// half command to run.
    #[test]
    fn zsh_history_keeps_timestamps_and_rejoins_continued_commands() {
        let imports = parse_shell_history(
            "zsh",
            ": 1700000000:0;echo one\n: 1700000060:0;git commit -m \\\nmessage\n",
        );
        assert_eq!(commands(&imports), ["echo one", "git commit -m \nmessage"]);
        assert_eq!(imports[0].timestamp, 1_700_000_000);
        assert_eq!(imports[1].timestamp, 1_700_000_060);
    }

    /// An extended-history file that has never had timestamps written is still
    /// a list of commands, at timestamp zero.
    #[test]
    fn zsh_history_without_timestamps_is_still_read() {
        let imports = parse_shell_history("zsh", "ls -la\n\ncargo test\n");
        assert_eq!(commands(&imports), ["ls -la", "cargo test"]);
        assert!(imports.iter().all(|i| i.timestamp == 0));
    }

    /// bash writes the epoch on its own `#`-prefixed line before the command.
    #[test]
    fn bash_history_pairs_each_command_with_the_epoch_above_it() {
        let imports =
            parse_shell_history("bash", "#1700000000\necho one\n#1700000060\nmake\nall\n");
        assert_eq!(commands(&imports), ["echo one", "make\nall"]);
        assert_eq!(imports[0].timestamp, 1_700_000_000);
        assert_eq!(imports[1].timestamp, 1_700_000_060);
    }

    /// `#`-comments are not timestamps, and a file with no timestamps at all is
    /// a plain command list.
    #[test]
    fn bash_history_without_timestamps_reads_every_line_as_a_command() {
        let imports = parse_shell_history("bash", "ls\n\ncargo build\n");
        assert_eq!(commands(&imports), ["ls", "cargo build"]);
    }

    /// fish stores YAML-ish records and escapes newlines and backslashes.
    #[test]
    fn fish_history_unescapes_newlines_and_backslashes() {
        let imports = parse_shell_history(
            "fish",
            "- cmd: echo one\n  when: 1700000000\n- cmd: printf 'a\\nb' \\\\ c\n  when: 1700000060\n",
        );
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].command, "echo one");
        assert_eq!(imports[0].timestamp, 1_700_000_000);
        assert_eq!(imports[1].command, "printf 'a\nb' \\ c");
        assert_eq!(imports[1].timestamp, 1_700_000_060);
    }

    /// An unrecognised shell is read as zsh rather than yielding nothing.
    #[test]
    fn an_unknown_shell_falls_back_to_the_zsh_reader() {
        let imports = parse_shell_history("nushell", ": 1700000000:0;echo hi\n");
        assert_eq!(commands(&imports), ["echo hi"]);
    }

    /// A history file larger than the cap is read from its tail, and the
    /// partial first record is dropped — parsing half a record would import a
    /// command fragment as if it were a command.
    #[test]
    fn an_oversized_history_is_read_from_its_tail_without_a_partial_record() {
        let path = scratch("history");
        let mut text = String::new();
        for index in 0..400 {
            text.push_str(&format!(": 17000000{index:02}:0;echo {index}\n"));
        }
        fs::write(&path, &text).unwrap();

        let (whole, truncated) = read_tail(&path, 1024 * 1024).unwrap();
        assert!(!truncated, "a small file is read entire");
        assert_eq!(whole.len(), text.len());

        let (tail, truncated) = read_tail(&path, 512).unwrap();
        assert!(truncated, "a file past the cap is truncated");
        assert!(tail.len() <= 512);
        // `read_tail` already dropped the partial first line.
        assert!(!tail.starts_with("echo"), "{tail}");
    }

    /// Trimming to a boundary is per-shell, because each marks a record
    /// differently — and a shell whose boundary never appears yields nothing
    /// rather than a fragment.
    #[test]
    fn trimming_to_a_record_boundary_understands_each_shells_marker() {
        assert_eq!(
            trim_to_record_boundary("zsh", "ontinued\n: 1700000000:0;echo one\n"),
            ": 1700000000:0;echo one\n"
        );
        assert_eq!(
            trim_to_record_boundary("fish", "  when: 1\n- cmd: echo one\n"),
            "- cmd: echo one\n"
        );
        assert_eq!(
            trim_to_record_boundary("bash", "fragment\n#1700000000\necho one\n"),
            "#1700000000\necho one\n"
        );
        // No boundary at all: zsh and fish give up rather than import a
        // fragment; bash keeps the text because it legitimately has none.
        assert_eq!(trim_to_record_boundary("zsh", "just a fragment\n"), "");
        assert_eq!(
            trim_to_record_boundary("bash", "ls\ncargo test\n"),
            "ls\ncargo test\n"
        );
    }

    /// A preset cwd is expanded locally, never by a shell. The two forms that
    /// would let a preset run a command are refused outright.
    #[test]
    fn a_preset_cwd_never_executes_shell_syntax() {
        for dangerous in ["$(rm -rf /)", "`whoami`", "/tmp/$(id)"] {
            let error =
                resolve_preset_cwd(dangerous).expect_err("command substitution must be refused");
            assert!(
                error.to_string().contains("execute shell syntax"),
                "{error}"
            );
        }
    }

    /// `origin` and an empty value both mean "wherever the user is", which is
    /// not a path to resolve.
    #[test]
    fn a_preset_cwd_of_origin_resolves_to_no_path() {
        assert_eq!(resolve_preset_cwd("origin").unwrap(), None);
        assert_eq!(resolve_preset_cwd("").unwrap(), None);
    }

    /// A path that is not absolute, or does not exist, is refused with the
    /// reason — a preset that silently falls back would run somewhere else.
    #[test]
    fn a_preset_cwd_must_be_an_absolute_directory_that_exists() {
        let relative = resolve_preset_cwd("work/api").unwrap_err();
        assert!(relative.to_string().contains("absolute"), "{relative}");

        let missing = resolve_preset_cwd("/definitely/not/a/real/directory").unwrap_err();
        assert!(missing.to_string().contains("does not exist"), "{missing}");
    }

    /// `$VAR` and `${VAR}` expand from the environment, and an undefined name
    /// expands to nothing rather than being left as a literal `$NAME` for a
    /// shell to interpret later.
    ///
    /// Deliberately reads `HOME` rather than setting a variable of its own:
    /// `env::set_var` mutates the whole process, and these tests run in parallel
    /// threads.
    #[test]
    fn a_preset_cwd_expands_both_variable_forms() {
        let home = env::var("HOME").expect("tests run with HOME set");
        for form in ["$HOME", "${HOME}"] {
            assert_eq!(
                resolve_preset_cwd(form).unwrap().as_deref(),
                Some(home.as_str()),
                "{form} did not expand"
            );
        }

        // An undefined variable expands to nothing rather than being passed on
        // as a literal `$NAME` for something else to interpret. Note what that
        // leaves: `$UNSET/x` becomes `/x`, which is still absolute, so it is the
        // existence check — not the absolute-path check — that refuses it.
        let error = resolve_preset_cwd("$SWITCHBOARD_DEFINITELY_UNSET_NAME/x").unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");
        // A genuinely relative preset is the case the absolute-path check is for.
        let error = resolve_preset_cwd("work/api").unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error}");
    }

    /// `~` alone is the home directory.
    #[test]
    fn a_preset_cwd_of_tilde_is_the_home_directory() {
        let home = env::var("HOME").expect("tests run with HOME set");
        assert_eq!(
            resolve_preset_cwd("~").unwrap().as_deref(),
            Some(home.as_str())
        );
    }
}
