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
    let Ok(bytes) = fs::read(path) else {
        return BTreeSet::new();
    };
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> BTreeSet<String> {
    serde_json::from_slice::<Vec<String>>(bytes)
        .ok()
        .unwrap_or_default()
        .into_iter()
        .filter(|slug| !slug.is_empty())
        .collect()
}

/// Add or remove one slug as a single locked read-modify-write.
///
/// Archiving used to load the set, change it, and write the whole file back as
/// two unsynchronized steps, on one fixed `.tmp` sibling. Two Git panes
/// archiving different reviews at the same moment would each write their own
/// idea of the whole set, and the later one erased the earlier one's change.
/// [`crate::state::update_private`] holds the lock across the entire
/// transaction, which is the same guarantee Projects stars already had.
fn set_at(path: &Path, slug: &str, archived: bool) -> Result<()> {
    crate::state::update_private(path, |current| {
        let mut slugs = current.map(parse).unwrap_or_default();
        if archived {
            slugs.insert(slug.to_string());
        } else {
            slugs.remove(slug);
        }
        let bytes = serde_json::to_vec_pretty(&slugs).context("serialize archived reviews")?;
        Ok((bytes, ()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A path no other test can collide with.
    ///
    /// The nanosecond clock alone was not enough: tests run in parallel threads
    /// and macOS does not hand out a distinct nanosecond per call, so two
    /// fixtures could land on one directory — and one of these tests puts a
    /// *directory* where another expects a file.
    fn test_path(name: &str) -> std::path::PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir()
            .join(format!(
                "switchboard-review-archive-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
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

    /// Archiving used to be a load and a later write with nothing holding the
    /// two together, so two Git panes archiving different reviews at the same
    /// moment each wrote their own idea of the whole set and the later one
    /// erased the earlier one's change.
    #[test]
    fn concurrent_panes_keep_every_completed_archive() {
        use std::sync::{Arc, Barrier};

        let path = test_path("archive.json");
        let slugs: Vec<String> = (0..8).map(|index| format!("session-{index}")).collect();
        let ready = Arc::new(Barrier::new(slugs.len() + 1));

        let workers: Vec<_> = slugs
            .iter()
            .cloned()
            .map(|slug| {
                let path = path.clone();
                let ready = Arc::clone(&ready);
                std::thread::spawn(move || {
                    ready.wait();
                    set_at(&path, &slug, true).unwrap();
                })
            })
            .collect();

        ready.wait();
        for worker in workers {
            worker.join().unwrap();
        }

        let archived = load_from(&path);
        for slug in &slugs {
            assert!(archived.contains(slug), "{slug} was lost: {archived:?}");
        }
        assert!(fs::read_dir(path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
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

    /// The public verbs, which the test build points at its scratch state.
    #[test]
    fn the_public_verbs_archive_and_restore_in_the_state_dir() {
        let slug = format!("swb-archive-public-{}", std::process::id());
        set(&slug, true).unwrap();
        assert!(load().contains(&slug));
        set(&slug, false).unwrap();
        assert!(!load().contains(&slug));
    }
}
