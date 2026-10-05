//! Open history: a tiny state file recording when each entry was last opened,
//! so the switcher can offer a "latest opened" (recency) sort. Keyed on
//! `Entry.id` (repo path / terminal id / workspace id); repo ids are stable so
//! they benefit most, ephemeral agent/workspace ids simply age out.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::state::{self, now, state_file};

/// Keep at most this many entries; oldest are dropped on write.
const CAP: usize = 200;

/// Location of the recency file: `$XDG_STATE_HOME/herdr-switchboard/recent.tsv`,
/// falling back to `~/.local/state/herdr-switchboard/recent.tsv`.
fn path() -> Option<PathBuf> {
    state_file("recent.tsv")
}

/// Load the id → last-opened-epoch map. Missing/unreadable file → empty map.
pub fn load() -> HashMap<String, u64> {
    path().map(|p| load_at(&p)).unwrap_or_default()
}

fn load_at(path: &std::path::Path) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    if let Ok(text) = fs::read_to_string(path) {
        parse(&text, &mut map);
    }
    map
}

/// Parse `epoch\tid` lines into `map`, keeping the newest timestamp per id.
fn parse(text: &str, map: &mut HashMap<String, u64>) {
    for line in text.lines() {
        if let Some((ts, id)) = line.split_once('\t') {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            if let Ok(ts) = ts.trim().parse::<u64>() {
                let slot = map.entry(id.to_string()).or_insert(0);
                *slot = (*slot).max(ts);
            }
        }
    }
}

/// Record that `id` was just opened (upsert to now), capped to the newest CAP.
pub fn touch(id: &str) {
    if let Some(path) = path() {
        touch_at(&path, id);
    }
}

fn touch_at(path: &std::path::Path, id: &str) {
    if id.is_empty() {
        return;
    }
    let id = id.to_string();
    edit_at(path, move |map| {
        map.insert(id, now());
        true
    });
}

/// Drop `id` from history (e.g. when a repo is removed).
pub fn forget(id: &str) {
    if let Some(path) = path() {
        forget_at(&path, id);
    }
}

fn forget_at(path: &std::path::Path, id: &str) {
    let id = id.to_string();
    edit_at(path, move |map| map.remove(&id).is_some());
}

/// Apply one change to the recency map and persist it, if it changed anything.
///
/// The read and the write are one locked transaction rather than a `load` and a
/// later `write`. Two Switchboard panes opening entries at the same moment were
/// each reading the map, adding their own row, and writing the whole file back —
/// so whichever landed second silently erased the other's row. They also raced
/// on one fixed `recent.tmp`, which could leave the real file truncated.
///
/// Failure is silence, as everywhere else here: recency is a convenience, and
/// nothing about opening a project should depend on it.
fn edit_at(path: &std::path::Path, change: impl FnOnce(&mut HashMap<String, u64>) -> bool) {
    let _ = state::update_private(path, |current| {
        let mut map = HashMap::new();
        if let Some(text) = current.and_then(|bytes| std::str::from_utf8(bytes).ok()) {
            parse(text, &mut map);
        }
        if !change(&mut map) {
            // Nothing to record. Write the file back as it was rather than
            // inventing a new one.
            return Ok((current.unwrap_or_default().to_vec(), ()));
        }
        Ok((serialize(&map), ()))
    });
}

/// The newest CAP entries as `epoch\tid` lines, newest first.
fn serialize(map: &HashMap<String, u64>) -> Vec<u8> {
    let mut rows: Vec<(&String, &u64)> = map.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    rows.truncate(CAP);

    let mut out = String::with_capacity(rows.len() * 48);
    for (id, ts) in rows {
        out.push_str(&ts.to_string());
        out.push('\t');
        out.push_str(id);
        out.push('\n');
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A path no other test can collide with. A wall-clock nonce is not enough:
    /// tests run in parallel threads and can share a tick.
    fn test_path() -> PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir()
            .join(format!(
                "switchboard-history-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ))
            .join("recent.tsv")
    }

    /// Every pane that opens something records it. They used to do that as a
    /// `load` and a later `write` with nothing in between, so two panes opening
    /// entries at the same moment each wrote their own idea of the whole file
    /// and the later one erased the earlier one's row — a recency list that
    /// silently forgets what you just opened.
    #[test]
    fn concurrent_panes_keep_every_recorded_open() {
        use std::sync::{Arc, Barrier};

        let path = test_path();
        let ids: Vec<String> = (0..8).map(|index| format!("gh/repo-{index}")).collect();
        let ready = Arc::new(Barrier::new(ids.len() + 1));

        let workers: Vec<_> = ids
            .iter()
            .cloned()
            .map(|id| {
                let path = path.clone();
                let ready = Arc::clone(&ready);
                std::thread::spawn(move || {
                    ready.wait();
                    edit_at(&path, move |map| {
                        map.insert(id, 100);
                        true
                    });
                })
            })
            .collect();

        ready.wait();
        for worker in workers {
            worker.join().unwrap();
        }

        let text = fs::read_to_string(&path).unwrap();
        let mut recorded = HashMap::new();
        parse(&text, &mut recorded);
        for id in &ids {
            assert!(recorded.contains_key(id), "{id} was lost:\n{text}");
        }
        // And no fixed-name tempfile survived to be raced over next time.
        assert!(fs::read_dir(path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A `forget` for an id that was never recorded must leave the file exactly
    /// as it found it, not rewrite it from a map it just built.
    #[test]
    fn forgetting_an_unknown_id_leaves_the_file_untouched() {
        let path = test_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "100\tgh/kept\n").unwrap();

        edit_at(&path, |map| map.remove("gh/absent").is_some());

        assert_eq!(fs::read_to_string(&path).unwrap(), "100\tgh/kept\n");
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn parse_keeps_newest_per_id() {
        let mut map = HashMap::new();
        parse("100\ta\n50\tb\n200\ta\n\tempty\nbad\tx\n", &mut map);
        assert_eq!(map.get("a"), Some(&200)); // newest wins
        assert_eq!(map.get("b"), Some(&50));
        assert!(!map.contains_key("empty")); // blank id skipped
        assert!(!map.contains_key("x")); // unparseable ts skipped
    }

    /// The public verbs against a scratch file: touch records, forget drops,
    /// an empty id is ignored, and a missing file loads as empty.
    #[test]
    fn touch_and_forget_round_trip_through_a_scratch_file() {
        let dir = std::env::temp_dir().join(format!("swb-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recent.tsv");
        assert!(load_at(&path).is_empty());

        touch_at(&path, "gh/a");
        touch_at(&path, "");
        assert_eq!(load_at(&path).len(), 1);
        forget_at(&path, "gh/a");
        assert!(load_at(&path).is_empty());
        let _ = fs::remove_dir_all(&dir);
        let _ = load();
    }
}
