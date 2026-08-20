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
