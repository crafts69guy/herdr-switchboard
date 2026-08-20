//! Comment-preserving, namespaced settings persistence.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use crate::data::Config;

#[cfg(test)]
pub(super) fn write_setting(path: &PathBuf, key: &str, value: &str) -> Result<()> {
    let mut doc = load_document(path)?;
    set_document_value(&mut doc, key, value)?;
    write_document(path, doc)
}

pub(super) fn load_document(path: &PathBuf) -> Result<toml_edit::DocumentMut> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    Ok(existing.parse::<toml_edit::DocumentMut>()?)
}

pub(super) fn set_document_value(
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    value: &str,
) -> Result<()> {
    let (section, field) = setting_path(key);
    if doc.get(section).is_none() {
        doc[section] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if is_bool_setting(key) {
        doc[section][field] = toml_edit::value(value == "true");
    } else if matches!(
        key,
        "history_limit"
            | "refresh_interval_ms"
            | "zen_width"
            | "usage_warn_percent"
            | "usage_alert_percent"
            | "all_files_warn"
    ) {
        doc[section][field] = toml_edit::value(value.parse::<i64>()?);
    } else {
        doc[section][field] = toml_edit::value(value);
    }
    Ok(())
}

pub(super) fn write_document(path: &PathBuf, doc: toml_edit::DocumentMut) -> Result<()> {
    let out = doc.to_string();
    Config::parse(&out)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub(super) fn setting_path(key: &str) -> (&'static str, &str) {
    match key {
        "keymode"
        | "notifications"
        | "notification_position"
        | "notification_sound"
        | "title_color"
        | "transparency"
        | "update_check" => ("common", key),
        "clone_source" => ("clone", "source"),
        "open_after_clone" => ("clone", "open_after"),
        "base_branch" | "all_files_warn" => ("git", key),
        "history_limit" => ("commands", key),
        "command_sort" => ("commands", "sort"),
        "refresh_interval_ms" => ("ports", key),
        "usage_warn_percent" => ("usage", "warn_percent"),
        "usage_alert_percent" => ("usage", "alert_percent"),
        "zen_width" => ("zen", "width"),
        "zen_scrim" => ("zen", "scrim"),
        "zen_scrim_color" => ("zen", "scrim_color"),
        "zen_chrome" => ("zen", "chrome"),
        _ => ("projects", key),
    }
}

fn is_bool_setting(key: &str) -> bool {
    matches!(
        key,
        "notifications"
            | "update_check"
            | "include_agents"
            | "include_workspaces"
            | "include_worktrees"
            | "preview_readme"
            | "open_after_clone"
            | "zen_scrim"
    )
}

pub(super) fn config_path() -> PathBuf {
    crate::config::config_path()
}
