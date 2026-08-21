//! Switchboard-owned visibility state for saved tuicr reviews.
//!
//! tuicr remains the source of truth for sessions and comments. This file stores
//! only session slugs that the user chose to hide from the active review list;
//! it never reads or mutates tuicr's session JSON.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

const STATE_FILE: &str = "archived-reviews.json";

/// The archived session slugs. Missing or malformed state degrades to no archive.
pub(super) fn load() -> BTreeSet<String> {
    let Some(path) = crate::state::state_file(STATE_FILE) else {
        return BTreeSet::new();
    };
    load_from(&path)
}

/// Add or remove one slug, replacing the state file atomically.
pub(super) fn set(slug: &str, archived: bool) -> Result<()> {
    let path = crate::state::state_file(STATE_FILE)
        .ok_or_else(|| anyhow!("no state directory is available"))?;
    set_at(&path, slug, archived)
}

fn load_from(path: &Path) -> BTreeSet<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    serde_json::from_str::<Vec<String>>(&text)
        .ok()
        .unwrap_or_default()
        .into_iter()
        .filter(|slug| !slug.is_empty())
        .collect()
}

fn set_at(path: &Path, slug: &str, archived: bool) -> Result<()> {
    let mut slugs = load_from(path);
    if archived {
        slugs.insert(slug.to_string());
    } else {
        slugs.remove(slug);
    }

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&slugs).context("serialize archived reviews")?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_path(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock is after epoch")
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "switchboard-review-archive-{}-{nonce}",
                std::process::id()
            ))
            .join(name)
    }

    #[test]
    fn missing_and_malformed_state_are_empty() {
        let path = test_path("archive.json");
        assert!(load_from(&path).is_empty());

        fs::create_dir_all(path.parent().expect("test path has a parent")).unwrap();
        fs::write(&path, "not json").unwrap();
        assert!(load_from(&path).is_empty());
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn archive_and_restore_replace_sorted_state() {
        let path = test_path("archive.json");
        set_at(&path, "z-session", true).unwrap();
        set_at(&path, "a-session", true).unwrap();

        assert_eq!(
            load_from(&path),
            BTreeSet::from(["a-session".to_string(), "z-session".to_string()])
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.find("a-session").unwrap() < text.find("z-session").unwrap());

        set_at(&path, "a-session", false).unwrap();
        assert_eq!(load_from(&path), BTreeSet::from(["z-session".to_string()]));
        assert!(!path.with_extension("tmp").exists());
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_directory_at_the_file_path_reports_a_write_error() {
        let path = test_path("archive.json");
        fs::create_dir_all(&path).unwrap();
        let error = set_at(&path, "session", true).unwrap_err();
        assert!(error.to_string().contains("replace"), "{error:#}");
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
