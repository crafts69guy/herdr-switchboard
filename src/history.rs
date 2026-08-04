//! Open history: a tiny state file recording when each entry was last opened,
//! so the switcher can offer a "latest opened" (recency) sort. Keyed on
//! `Entry.id` (repo path / terminal id / workspace id); repo ids are stable so
//! they benefit most, ephemeral agent/workspace ids simply age out.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::state::{now, state_file};

/// Keep at most this many entries; oldest are dropped on write.
const CAP: usize = 200;

/// Location of the recency file: `$XDG_STATE_HOME/herdr-switchboard/recent.tsv`,
/// falling back to `~/.local/state/herdr-switchboard/recent.tsv`.
fn path() -> Option<PathBuf> {
    state_file("recent.tsv")
}

/// Load the id → last-opened-epoch map. Missing/unreadable file → empty map.
pub fn load() -> HashMap<String, u64> {
    let mut map = HashMap::new();
    let Some(p) = path() else { return map };
    let Ok(text) = fs::read_to_string(p) else {
        return map;
    };
    parse(&text, &mut map);
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
    if id.is_empty() {
        return;
    }
    let mut map = load();
    map.insert(id.to_string(), now());
    write(&map);
}

/// Drop `id` from history (e.g. when a repo is removed).
pub fn forget(id: &str) {
    let mut map = load();
    if map.remove(id).is_some() {
        write(&map);
    }
}

/// Serialize the newest CAP entries and write atomically (temp file + rename).
fn write(map: &HashMap<String, u64>) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        if fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let mut rows: Vec<(&String, &u64)> = map.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    rows.truncate(CAP);

    let mut out = String::with_capacity(rows.len() * 48);
    for (id, ts) in rows {
        out.push_str(&ts.to_string());
        out.push('\t');
        out.push_str(id);
        out.push('\n');
    }

    let tmp = p.with_extension("tmp");
    if fs::write(&tmp, out).is_ok() {
        let _ = fs::rename(&tmp, &p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keeps_newest_per_id() {
        let mut map = HashMap::new();
        parse("100\ta\n50\tb\n200\ta\n\tempty\nbad\tx\n", &mut map);
        assert_eq!(map.get("a"), Some(&200)); // newest wins
        assert_eq!(map.get("b"), Some(&50));
        assert!(!map.contains_key("empty")); // blank id skipped
        assert!(!map.contains_key("x")); // unparseable ts skipped
    }
}
