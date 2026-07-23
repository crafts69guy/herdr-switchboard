//! Data layer: theme, plugin config, and the unified entry list (agents,
//! workspaces, ghq repos, and linked Git worktrees).

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::PathBuf;

use ratatui::style::Color;

use crate::runner::CommandRunner;

// --- theme -----------------------------------------------------------------

/// Colours pulled from herdr's `[theme.custom]` so the TUI matches the terminal.
/// The default is empty — every slot then falls back to its ratatui colour,
/// which is also what a user with no `[theme.custom]` section gets.
#[derive(Clone, Default)]
pub struct Theme {
    slots: HashMap<String, Color>,
}

impl Theme {
    pub fn load() -> Self {
        let path = env::var("HERDR_CONFIG_PATH").unwrap_or_else(|_| {
            format!(
                "{}/.config/herdr/config.toml",
                env::var("HOME").unwrap_or_default()
            )
        });
        let mut slots = HashMap::new();
        if let Ok(text) = fs::read_to_string(path) {
            let mut in_section = false;
            for line in text.lines() {
                let t = line.trim();
                if t.starts_with('[') {
                    in_section = t == "[theme.custom]";
                    continue;
                }
                if !in_section {
                    continue;
                }
                if let Some((k, v)) = t.split_once('=') {
                    if let Some(color) = parse_hex(v.trim()) {
                        slots.insert(k.trim().to_string(), color);
                    }
                }
            }
        }
        Theme { slots }
    }

    pub fn get(&self, key: &str) -> Option<Color> {
        self.slots.get(key).copied()
    }

    /// Build a theme from `slot = #rrggbb` pairs, for tests that want a specific
    /// palette without writing a herdr config to disk.
    #[cfg(test)]
    pub fn from_slots(pairs: &[(&str, &str)]) -> Self {
        let slots = pairs
            .iter()
            .filter_map(|(k, v)| parse_hex(v).map(|c| (k.to_string(), c)))
            .collect();
        Theme { slots }
    }

    pub fn or(&self, key: &str, fallback: Color) -> Color {
        self.get(key).unwrap_or(fallback)
    }

    /// Resolve a colour spec that is either a `[theme.custom]` slot name
    /// (e.g. `peach`) or a literal `#rrggbb`.
    pub fn resolve(&self, spec: &str) -> Option<Color> {
        if spec.starts_with('#') {
            parse_hex(spec)
        } else {
            self.get(spec)
        }
    }
}

fn parse_hex(raw: &str) -> Option<Color> {
    let s = raw.trim().trim_matches('"');
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

// --- plugin config ---------------------------------------------------------

/// Flat `key = value` config from the plugin's config dir.
/// The default is empty, which is what an unconfigured plugin has: every
/// `get`/`bool` then answers with the caller's own default.
#[derive(Clone, Default)]
pub struct Config {
    map: HashMap<String, String>,
}

impl Config {
    pub fn load() -> Self {
        let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").unwrap_or_default();
        let mut map = HashMap::new();
        if !dir.is_empty() {
            if let Ok(text) = fs::read_to_string(PathBuf::from(dir).join("config.toml")) {
                for line in text.lines() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') || t.starts_with('[') {
                        continue;
                    }
                    if let Some((k, v)) = t.split_once('=') {
                        let v = v.trim();
                        let v = v.split_once('#').map(|(a, _)| a.trim()).unwrap_or(v);
                        map.insert(k.trim().to_string(), v.trim_matches('"').to_string());
                    }
                }
            }
        }
        Config { map }
    }

    pub fn get(&self, key: &str, default: &str) -> String {
        match self.map.get(key) {
            Some(v) if !v.is_empty() => v.clone(),
            _ => default.to_string(),
        }
    }

    pub fn bool(&self, key: &str, default: bool) -> bool {
        self.get(key, if default { "true" } else { "false" }) == "true"
    }

    /// Build a config from key/value pairs, for tests that want a specific flag.
    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Config {
            map: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

// --- entries ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Agent,
    Workspace,
    Repo,
    Worktree,
}

impl Kind {
    /// Stable ordering used by the "Kind" sort (agents first, worktrees last).
    pub fn order(self) -> u8 {
        match self {
            Kind::Agent => 0,
            Kind::Workspace => 1,
            Kind::Repo => 2,
            Kind::Worktree => 3,
        }
    }
}

/// How the no-query browse list is ordered. Fuzzy score always wins while the
/// user is typing; this only decides the resting order.
#[derive(Clone, Copy, PartialEq)]
pub enum SortMode {
    /// Latest opened first (default), from the recency history file.
    Recent,
    /// Alphabetical by the primary column.
    Name,
    /// Grouped by kind: agents, workspaces, repos, then linked worktrees.
    Kind,
}

impl SortMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "name" => SortMode::Name,
            "kind" => SortMode::Kind,
            _ => SortMode::Recent,
        }
    }

    pub fn next(self) -> Self {
        match self {
            SortMode::Recent => SortMode::Name,
            SortMode::Name => SortMode::Kind,
            SortMode::Kind => SortMode::Recent,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortMode::Recent => "recent",
            SortMode::Name => "name",
            SortMode::Kind => "kind",
        }
    }
}

/// Which group the list is narrowed to. `All` blends every source.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GroupFilter {
    All,
    Only(Kind),
}

impl GroupFilter {
    /// Config vocabulary for the tab selected at startup / settings apply.
    /// Unknown values are deliberately lenient and land on `All`.
    pub fn parse(s: &str) -> Self {
        match s {
            "agents" => GroupFilter::Only(Kind::Agent),
            "workspaces" => GroupFilter::Only(Kind::Workspace),
            "repos" => GroupFilter::Only(Kind::Repo),
            "worktrees" => GroupFilter::Only(Kind::Worktree),
            _ => GroupFilter::All,
        }
    }

    /// Does `kind` pass this filter?
    pub fn matches(self, kind: Kind) -> bool {
        match self {
            GroupFilter::All => true,
            GroupFilter::Only(k) => k == kind,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GroupFilter::All => "All",
            GroupFilter::Only(Kind::Agent) => "Agents",
            GroupFilter::Only(Kind::Workspace) => "Workspaces",
            GroupFilter::Only(Kind::Repo) => "Repos",
            GroupFilter::Only(Kind::Worktree) => "Worktrees",
        }
    }
}

#[derive(Clone)]
pub struct Entry {
    pub kind: Kind,
    /// Target id: terminal id, workspace id, ghq relative path, or worktree path.
    pub id: String,
    /// Absolute directory when one applies (repo path, agent cwd).
    pub dir: Option<String>,
    /// Human label used for workspace/tab names and confirmations.
    pub label: String,
    // Display columns.
    pub icon: String,
    pub icon_color: Color,
    pub primary: String,
    pub secondary: String,
    /// Plain text used for fuzzy matching.
    pub search: String,
}

pub fn ghq_root(runner: &dyn CommandRunner) -> String {
    runner.capture("ghq", &["root"]).unwrap_or_default()
}

/// Status → colour, shared with the preview card so an agent's pill there is the
/// same colour as its bullet in the list.
pub fn state_color(theme: &Theme, status: &str) -> Color {
    match status {
        "idle" | "ready" | "done" => theme.or("green", Color::Green),
        "working" => theme.or("yellow", Color::Yellow),
        "blocked" => theme.or("red", Color::Red),
        _ => theme.or("subtext0", Color::DarkGray),
    }
}

/// Running herdr agents as entries. The include toggle lives on the source
/// (`AgentSource::enabled`), so this just loads.
pub fn load_agents(runner: &dyn CommandRunner, theme: &Theme) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(json) = runner.capture("herdr", &["agent", "list"]) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            if let Some(arr) = v["result"]["agents"].as_array() {
                for a in arr {
                    // herdr keys `agent focus`/`agent get` on the pane id, not the
                    // terminal id, so that is the entry's target. A row without one
                    // cannot be focused, so skip it.
                    let pane = a["pane_id"].as_str().unwrap_or("").to_string();
                    if pane.is_empty() {
                        continue;
                    }
                    // herdr can report a pane with no agent label (a stale or
                    // half-detected entry). Those are not agents.
                    let Some(agent) = a["agent"].as_str().filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    let status = a["agent_status"].as_str().unwrap_or("unknown");
                    let cwd = a["foreground_cwd"]
                        .as_str()
                        .or_else(|| a["cwd"].as_str())
                        .unwrap_or("")
                        .to_string();
                    let base = basename(&cwd);
                    entries.push(Entry {
                        kind: Kind::Agent,
                        id: pane,
                        dir: if cwd.is_empty() { None } else { Some(cwd) },
                        label: base.clone(),
                        icon: "●".into(),
                        icon_color: state_color(theme, status),
                        primary: format!("{base} · {agent}"),
                        secondary: status.to_string(),
                        search: format!("{base} {agent} {status}"),
                    });
                }
            }
        }
    }
    entries
}

/// Open herdr workspaces as entries.
pub fn load_workspaces(runner: &dyn CommandRunner, theme: &Theme) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(json) = runner.capture("herdr", &["workspace", "list"]) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            if let Some(arr) = v["result"]["workspaces"].as_array() {
                for w in arr {
                    let wid = w["workspace_id"].as_str().unwrap_or("").to_string();
                    if wid.is_empty() {
                        continue;
                    }
                    let label = w["label"].as_str().unwrap_or("workspace").to_string();
                    let num = w["number"].as_i64().unwrap_or(0);
                    let panes = w["pane_count"].as_i64().unwrap_or(0);
                    let focused = w["focused"].as_bool().unwrap_or(false);
                    let mut sec = format!("#{num} · {panes}p");
                    if focused {
                        sec.push_str(" · current");
                    }
                    entries.push(Entry {
                        kind: Kind::Workspace,
                        id: wid,
                        dir: None,
                        label: label.clone(),
                        icon: "".into(),
                        icon_color: theme.or("accent", Color::Cyan),
                        primary: label.clone(),
                        secondary: sec,
                        search: format!("{label} workspace"),
                    });
                }
            }
        }
    }
    entries
}

/// Every `ghq` repository as an entry, rooted at `root`.
pub fn load_repos(runner: &dyn CommandRunner, theme: &Theme, root: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(list) = runner.capture("ghq", &["list"]) {
        for rel in list.lines().filter(|l| !l.is_empty()) {
            let (host, rest) = rel.split_once('/').unwrap_or(("", rel));
            let (icon, color) = host_icon(host, theme);
            let short = host.split('.').next().unwrap_or(host).to_string();
            entries.push(Entry {
                kind: Kind::Repo,
                id: rel.to_string(),
                dir: Some(format!("{root}/{rel}")),
                label: basename(rel),
                icon: icon.into(),
                icon_color: color,
                primary: rest.to_string(),
                secondary: short.clone(),
                search: format!("{rel} {short}"),
            });
        }
    }
    entries
}

#[derive(Default)]
struct WorktreeRecord {
    path: String,
    head: String,
    branch: Option<String>,
    prunable: bool,
}

/// Parse `git worktree list --porcelain -z`. Git promises this format is stable;
/// NUL fields also preserve paths and reasons containing whitespace or newlines.
fn parse_worktree_list(raw: &str) -> Vec<WorktreeRecord> {
    let mut records = Vec::new();
    let mut current = WorktreeRecord::default();

    for field in raw.split('\0') {
        if field.is_empty() {
            if !current.path.is_empty() {
                records.push(current);
                current = WorktreeRecord::default();
            }
            continue;
        }
        if let Some(path) = field.strip_prefix("worktree ") {
            current.path = path.to_string();
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            current.head = head.to_string();
        } else if let Some(branch) = field.strip_prefix("branch refs/heads/") {
            current.branch = Some(branch.to_string());
        } else if field == "prunable" || field.starts_with("prunable ") {
            current.prunable = true;
        }
    }
    if !current.path.is_empty() {
        records.push(current);
    }
    records
}

/// Linked worktrees attached to every ghq repository. The first porcelain record
/// is the main worktree, already represented by the Repos source, so it is skipped.
pub fn load_worktrees(runner: &dyn CommandRunner, theme: &Theme, root: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let Some(repos) = runner.capture("ghq", &["list"]) else {
        return entries;
    };

    for rel in repos.lines().filter(|line| !line.is_empty()) {
        let repo = format!("{}/{rel}", root.trim_end_matches('/'));
        let Some(raw) = runner.capture(
            "git",
            &["-C", &repo, "worktree", "list", "--porcelain", "-z"],
        ) else {
            continue;
        };
        let (host, rest) = rel.split_once('/').unwrap_or(("", rel));
        let (icon, color) = host_icon(host, theme);

        for record in parse_worktree_list(&raw).into_iter().skip(1) {
            if record.prunable
                || !std::path::Path::new(&record.path).is_dir()
                || !seen.insert(record.path.clone())
            {
                continue;
            }
            let branch = record.branch.unwrap_or_else(|| {
                let short: String = record.head.chars().take(8).collect();
                if short.is_empty() {
                    "detached".into()
                } else {
                    format!("detached@{short}")
                }
            });
            let label = basename(&record.path);
            entries.push(Entry {
                kind: Kind::Worktree,
                id: record.path.clone(),
                dir: Some(record.path.clone()),
                label,
                icon: icon.into(),
                icon_color: color,
                primary: rest.to_string(),
                secondary: branch.clone(),
                search: format!("{rel} {branch} {} worktree", record.path),
            });
        }
    }
    entries
}

fn host_icon(host: &str, theme: &Theme) -> (&'static str, Color) {
    match host {
        "github.com" => ("", theme.or("mauve", Color::Magenta)),
        "bitbucket.org" => ("", theme.or("blue", Color::Blue)),
        "gitlab.com" => ("", theme.or("peach", Color::Yellow)),
        _ => ("", theme.or("subtext0", Color::DarkGray)),
    }
}

fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    const AGENTS: &str = r#"{"result":{"agents":[
        {"pane_id":"w1:p1","terminal_id":"term-1","agent":"claude","agent_status":"working","foreground_cwd":"/home/u/proj"},
        {"pane_id":"","agent":"ghost","agent_status":"idle"},
        {"pane_id":"w1:p2","terminal_id":"term-2","agent":"","agent_status":"idle"}
    ]}}"#;
    const WORKSPACES: &str = r#"{"result":{"workspaces":[
        {"workspace_id":"ws-1","label":"work","number":2,"pane_count":3,"focused":true}
    ]}}"#;
    const REPOS: &str = "github.com/o/repo-a\nbitbucket.org/o/repo-b\n";

    #[test]
    fn group_filter_parses_config_and_falls_back_to_all() {
        assert_eq!(GroupFilter::parse("agents"), GroupFilter::Only(Kind::Agent));
        assert_eq!(
            GroupFilter::parse("worktrees"),
            GroupFilter::Only(Kind::Worktree)
        );
        assert_eq!(GroupFilter::parse("unknown"), GroupFilter::All);
    }

    #[test]
    fn load_agents_maps_json_and_drops_idless_and_labelless() {
        let runner = MockRunner::new().on("herdr agent list", AGENTS);
        let e = load_agents(&runner, &Theme::default());
        // The pane-less and label-less rows are dropped; only the real one stays.
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].kind, Kind::Agent);
        // The target is the pane id — what `herdr agent focus`/`get` accept.
        assert_eq!(e[0].id, "w1:p1");
        assert_eq!(e[0].dir.as_deref(), Some("/home/u/proj"));
        assert_eq!(e[0].primary, "proj · claude");
        assert_eq!(e[0].secondary, "working");
    }

    #[test]
    fn load_workspaces_marks_the_focused_one_current() {
        let runner = MockRunner::new().on("herdr workspace list", WORKSPACES);
        let e = load_workspaces(&runner, &Theme::default());
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].kind, Kind::Workspace);
        assert_eq!(e[0].id, "ws-1");
        assert!(e[0].secondary.contains("current"), "{:?}", e[0].secondary);
    }

    #[test]
    fn load_repos_splits_host_and_roots_the_dir() {
        let runner = MockRunner::new().on("ghq list", REPOS);
        let e = load_repos(&runner, &Theme::default(), "/root");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].kind, Kind::Repo);
        assert_eq!(e[0].id, "github.com/o/repo-a");
        assert_eq!(e[0].dir.as_deref(), Some("/root/github.com/o/repo-a"));
        assert_eq!(e[0].primary, "o/repo-a");
        assert_eq!(e[0].label, "repo-a");
    }

    #[test]
    fn load_worktrees_keeps_only_live_linked_records() {
        let dir = std::env::temp_dir().join(format!("ghq-worktrees-{}", std::process::id()));
        let linked = dir.join("feature branch\nodd");
        let detached = dir.join("detached");
        let stale = dir.join("stale");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir_all(&detached).unwrap();
        std::fs::create_dir_all(&stale).unwrap();

        let raw = format!(
            "worktree /root/github.com/o/repo-a\0HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0branch refs/heads/main\0\0worktree {}\0HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\0branch refs/heads/feature/nul-safe\0locked keep it\0\0worktree {}\0HEAD 1234567890abcdef1234567890abcdef12345678\0detached\0\0worktree {}\0HEAD cccccccccccccccccccccccccccccccccccccccc\0branch refs/heads/stale\0prunable missing gitdir\0\0",
            linked.display(),
            detached.display(),
            stale.display()
        );
        let runner = MockRunner::new()
            .on("ghq list", "github.com/o/repo-a\n")
            .on("git -C /root/github.com/o/repo-a worktree list", &raw);

        let e = load_worktrees(&runner, &Theme::default(), "/root");
        assert_eq!(e.len(), 2);
        assert!(e.iter().all(|entry| entry.kind == Kind::Worktree));
        assert_eq!(e[0].dir.as_deref(), linked.to_str());
        assert_eq!(e[0].primary, "o/repo-a");
        assert_eq!(e[0].secondary, "feature/nul-safe");
        assert_eq!(e[1].secondary, "detached@12345678");
        assert!(e[0].search.contains("feature branch\nodd"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn worktree_parser_preserves_unknown_fields_and_unterminated_last_record() {
        let records = parse_worktree_list(
            "worktree /main\0HEAD abc\0branch refs/heads/main\0future value\0\0worktree /linked\0HEAD def",
        );
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].path, "/linked");
        assert_eq!(records[1].head, "def");
    }

    #[test]
    fn a_source_that_returns_nothing_yields_no_entries() {
        // Unseeded: empty stdout is unparseable JSON / an empty repo list, so
        // each loader must come up empty rather than panic.
        assert!(load_agents(&MockRunner::new(), &Theme::default()).is_empty());
        assert!(load_workspaces(&MockRunner::new(), &Theme::default()).is_empty());
        assert!(load_repos(&MockRunner::new(), &Theme::default(), "/root").is_empty());
        assert!(load_worktrees(&MockRunner::new(), &Theme::default(), "/root").is_empty());
    }
}
