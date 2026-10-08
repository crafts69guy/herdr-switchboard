//! Data layer: theme, plugin config, and the unified entry list (agents,
//! workspaces, ghq repos, and linked Git worktrees).

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;

use ratatui::style::Color;

pub use crate::config::Config;
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
        fs::read_to_string(path)
            .map(|text| Self::from_herdr_config(&text))
            .unwrap_or_default()
    }

    /// The `[theme.custom]` hex slots out of herdr's config text; every other
    /// section, and any value that is not a `#rrggbb`, is ignored.
    fn from_herdr_config(text: &str) -> Self {
        let mut slots = HashMap::new();
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
    Starred,
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
            "starred" => GroupFilter::Starred,
            _ => GroupFilter::All,
        }
    }

    /// Does an entry with this kind and star state pass the filter?
    pub fn matches(self, kind: Kind, starred: bool) -> bool {
        match self {
            GroupFilter::All => true,
            GroupFilter::Only(k) => k == kind,
            GroupFilter::Starred => starred,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GroupFilter::All => "All",
            GroupFilter::Only(Kind::Agent) => "Agents",
            GroupFilter::Only(Kind::Workspace) => "Workspaces",
            GroupFilter::Only(Kind::Repo) => "Repos",
            GroupFilter::Only(Kind::Worktree) => "Worktrees",
            GroupFilter::Starred => "★ Starred",
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

/// Running herdr agents as entries. `ProjectCatalog` owns the inclusion policy,
/// so this function only parses the command output.
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

/// Take one snapshot of the ghq catalogue for both repositories and worktrees.
pub fn load_repo_names(runner: &dyn CommandRunner) -> Vec<String> {
    runner
        .capture("ghq", &["list"])
        .map(|list| {
            list.lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Every repository in a ghq snapshot, rooted at `root`.
pub fn load_repos(repos: &[String], theme: &Theme, root: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    for rel in repos {
        let (host, rest) = rel.split_once('/').unwrap_or(("", rel));
        let (icon, color) = host_icon(host, theme);
        let short = host.split('.').next().unwrap_or(host).to_string();
        entries.push(Entry {
            kind: Kind::Repo,
            id: rel.clone(),
            dir: Some(format!("{}/{rel}", root.trim_end_matches('/'))),
            label: basename(rel),
            icon: icon.into(),
            icon_color: color,
            primary: rest.to_string(),
            secondary: short.clone(),
            search: format!("{rel} {short}"),
        });
    }
    entries
}

#[derive(Default)]
pub(crate) struct WorktreeRecord {
    pub path: String,
    pub head: String,
    pub branch: Option<String>,
    pub prunable: bool,
    pub locked: bool,
}

/// Parse `git worktree list --porcelain -z`. Git promises this format is stable;
/// NUL fields also preserve paths and reasons containing whitespace or newlines.
pub(crate) fn parse_worktree_list(raw: &str) -> Vec<WorktreeRecord> {
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
        } else if field == "locked" || field.starts_with("locked ") {
            current.locked = true;
        }
    }
    if !current.path.is_empty() {
        records.push(current);
    }
    records
}

/// Whether a checkout could possibly have linked worktrees, cheaply enough to
/// ask before forking `git`.
///
/// Git records every linked worktree of an ordinary checkout as a directory
/// under `.git/worktrees/`, so an empty or absent one is a definitive no — and
/// answering it is a `read_dir` rather than a process spawn. That matters
/// because the probe runs once per ghq repository: on a machine with a few
/// hundred repositories and worktrees on a handful, this is the difference
/// between a few hundred `git` processes and a few.
///
/// It **fails open** in every case it cannot settle from the filesystem. A
/// `.git` file rather than a directory means this path is itself a linked
/// worktree or a submodule, whose administrative directory lives elsewhere; an
/// unreadable `.git/worktrees` could be a permissions problem rather than an
/// absence. Both still probe, because the cost of a wrong "no" is a worktree
/// silently missing from the catalogue, and the cost of a wrong "yes" is one
/// subprocess.
fn may_have_worktrees(repo: &str) -> bool {
    let git = std::path::Path::new(repo).join(".git");
    let Ok(metadata) = std::fs::metadata(&git) else {
        // No `.git` at all: not a checkout ghq can have worktrees for. Probing
        // would only make git report the same thing, slower.
        return false;
    };
    if !metadata.is_dir() {
        return true;
    }
    match std::fs::read_dir(git.join("worktrees")) {
        Ok(mut entries) => entries.next().is_some(),
        // Absent is a definitive no; unreadable for any other reason is not.
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Linked worktrees attached to every ghq repository. The first porcelain record
/// is the main worktree, already represented by the Repos source, so it is skipped.
pub fn load_worktrees(
    runner: &dyn CommandRunner,
    repos: &[String],
    theme: &Theme,
    root: &str,
) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    if repos.is_empty() {
        return entries;
    }

    const MAX_WORKTREE_PROBES: usize = 4;
    let worker_count = repos.len().min(MAX_WORKTREE_PROBES);
    let mut probes = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for worker in 0..worker_count {
            handles.push(scope.spawn(move || {
                let mut found = Vec::new();
                for index in (worker..repos.len()).step_by(worker_count) {
                    let rel = &repos[index];
                    let repo = format!("{}/{rel}", root.trim_end_matches('/'));
                    if !may_have_worktrees(&repo) {
                        continue;
                    }
                    if let Some(raw) = runner.capture(
                        "git",
                        &["-C", &repo, "worktree", "list", "--porcelain", "-z"],
                    ) {
                        found.push((index, raw));
                    }
                }
                found
            }));
        }
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .flatten()
            .collect::<Vec<_>>()
    });
    probes.sort_by_key(|(index, _)| *index);

    for (index, raw) in probes {
        let rel = &repos[index];
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
    use std::ffi::OsStr;
    use std::io;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::{ExitStatus, Output};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::runner::{CommandRunner, MockRunner};

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
        assert_eq!(GroupFilter::parse("starred"), GroupFilter::Starred);
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
        let repos = load_repo_names(&runner);
        let e = load_repos(&repos, &Theme::default(), "/root");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].kind, Kind::Repo);
        assert_eq!(e[0].id, "github.com/o/repo-a");
        assert_eq!(e[0].dir.as_deref(), Some("/root/github.com/o/repo-a"));
        assert_eq!(e[0].primary, "o/repo-a");
        assert_eq!(e[0].label, "repo-a");
    }

    /// A throwaway ghq root holding `count` checkouts. Each gets a real
    /// `.git/worktrees/` entry, which is the shape [`may_have_worktrees`] reads
    /// to decide whether the repository is worth a `git` process at all.
    fn ghq_root_with_registered_worktrees(tag: &str, count: usize) -> (PathBuf, Vec<String>) {
        let root = std::env::temp_dir().join(format!("ghq-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let repos = (0..count)
            .map(|index| {
                let rel = format!("github.com/o/repo-{index}");
                std::fs::create_dir_all(root.join(&rel).join(".git").join("worktrees").join("wt"))
                    .unwrap();
                rel
            })
            .collect();
        (root, repos)
    }

    #[test]
    fn load_worktrees_keeps_only_live_linked_records() {
        let (root, repos) = ghq_root_with_registered_worktrees("worktrees", 1);
        let dir = root.join("checkouts");
        let linked = dir.join("feature branch\nodd");
        let detached = dir.join("detached");
        let stale = dir.join("stale");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir_all(&detached).unwrap();
        std::fs::create_dir_all(&stale).unwrap();

        let raw = format!(
            "worktree /root/github.com/o/repo-0\0HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0branch refs/heads/main\0\0worktree {}\0HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\0branch refs/heads/feature/nul-safe\0locked keep it\0\0worktree {}\0HEAD 1234567890abcdef1234567890abcdef12345678\0detached\0\0worktree {}\0HEAD cccccccccccccccccccccccccccccccccccccccc\0branch refs/heads/stale\0prunable missing gitdir\0\0",
            linked.display(),
            detached.display(),
            stale.display()
        );
        let runner = MockRunner::new().on("worktree list", &raw);

        let e = load_worktrees(&runner, &repos, &Theme::default(), &root.to_string_lossy());
        assert_eq!(e.len(), 2);
        assert!(e.iter().all(|entry| entry.kind == Kind::Worktree));
        assert_eq!(e[0].dir.as_deref(), linked.to_str());
        assert_eq!(e[0].primary, "o/repo-0");
        assert_eq!(e[0].secondary, "feature/nul-safe");
        assert_eq!(e[1].secondary, "detached@12345678");
        assert!(e[0].search.contains("feature branch\nodd"));

        std::fs::remove_dir_all(&root).ok();
    }

    /// The probe is skipped for a checkout that has registered no worktrees, and
    /// that is the whole point: it runs once per ghq repository, so on a machine
    /// with hundreds of them the spawns saved are almost all of them.
    #[test]
    fn a_repository_with_no_registered_worktrees_is_never_probed() {
        let (root, mut repos) = ghq_root_with_registered_worktrees("skip", 1);
        let bare = "github.com/o/plain".to_string();
        std::fs::create_dir_all(root.join(&bare).join(".git")).unwrap();
        repos.push(bare);
        let runner = MockRunner::new();

        load_worktrees(&runner, &repos, &Theme::default(), &root.to_string_lossy());

        let probed: Vec<String> = runner
            .calls()
            .iter()
            .filter_map(|argv| {
                argv.iter()
                    .find(|arg| arg.contains("github.com/o/"))
                    .cloned()
            })
            .collect();
        assert_eq!(
            probed.len(),
            1,
            "only the checkout with a registered worktree is worth a process: {probed:?}"
        );
        assert!(probed[0].ends_with("repo-0"), "{probed:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The prefilter fails open. A wrong "no" loses a worktree from the
    /// catalogue silently; a wrong "yes" costs one subprocess. Only the two
    /// cases the filesystem settles outright answer no.
    #[test]
    fn the_worktree_prefilter_only_refuses_what_the_filesystem_settles() {
        let root = std::env::temp_dir().join(format!("ghq-prefilter-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let at = |name: &str| root.join(name).to_string_lossy().into_owned();

        // No `.git` at all, and a `.git` directory with no `worktrees/`: git
        // would only report the same absence, slower.
        std::fs::create_dir_all(root.join("not-a-checkout")).unwrap();
        std::fs::create_dir_all(root.join("plain").join(".git")).unwrap();
        assert!(!may_have_worktrees(&at("not-a-checkout")));
        assert!(!may_have_worktrees(&at("plain")));

        // A registered worktree.
        std::fs::create_dir_all(root.join("host").join(".git").join("worktrees").join("wt"))
            .unwrap();
        assert!(may_have_worktrees(&at("host")));

        // A `.git` *file* points at an administrative directory somewhere else —
        // this path is itself a linked worktree or a submodule, and nothing here
        // can rule out worktrees from the pointer alone.
        std::fs::create_dir_all(root.join("linked")).unwrap();
        std::fs::write(root.join("linked").join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert!(may_have_worktrees(&at("linked")));

        std::fs::remove_dir_all(&root).ok();
    }

    struct ProbeRunner {
        active: AtomicUsize,
        maximum: AtomicUsize,
    }

    impl ProbeRunner {
        fn new() -> Self {
            Self {
                active: AtomicUsize::new(0),
                maximum: AtomicUsize::new(0),
            }
        }
    }

    impl CommandRunner for ProbeRunner {
        fn output(&self, _program: &str, _args: &[&str]) -> io::Result<Output> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(Output {
                status: ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }

        fn status(&self, _program: &str, _args: &[&str]) -> io::Result<ExitStatus> {
            Ok(ExitStatus::from_raw(0))
        }

        fn output_stdin(&self, program: &str, args: &[&str], _stdin: &str) -> io::Result<Output> {
            self.output(program, args)
        }

        fn spawn_detached(&self, _program: &OsStr, _args: &[&str]) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn worktree_discovery_uses_at_most_four_parallel_probes() {
        let runner = ProbeRunner::new();
        let (root, repos) = ghq_root_with_registered_worktrees("probes", 8);

        let entries = load_worktrees(&runner, &repos, &Theme::default(), &root.to_string_lossy());

        assert!(entries.is_empty());
        let maximum = runner.maximum.load(Ordering::SeqCst);
        assert!(
            (2..=4).contains(&maximum),
            "maximum concurrency was {maximum}"
        );
        std::fs::remove_dir_all(&root).ok();
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
        assert!(load_repos(&[], &Theme::default(), "/root").is_empty());
        assert!(load_worktrees(&MockRunner::new(), &[], &Theme::default(), "/root").is_empty());
    }

    /// Only `[theme.custom]` hex values become slots.
    #[test]
    fn the_theme_reads_only_custom_hex_slots() {
        let theme = Theme::from_herdr_config(
            "[theme]\nname = \"terminal\"\naccent = \"#ffffff\"\n\
             [theme.custom]\naccent = \"#00CF6A\"\nbroken = \"blue\"\nno equals sign\n\
             [ui]\npanel_bg = \"#000000\"\n",
        );
        assert_eq!(theme.get("accent"), Some(Color::Rgb(0x00, 0xCF, 0x6A)));
        assert_eq!(theme.get("broken"), None);
        assert_eq!(theme.get("panel_bg"), None);
        let _ = Theme::load();
    }

    /// The probe runner's other verbs answer success without running anything.
    #[test]
    fn the_probe_runner_answers_every_verb() {
        let runner = ProbeRunner::new();
        assert!(runner.status("x", &[]).unwrap().success());
        assert!(runner
            .output_stdin("x", &[], "in")
            .unwrap()
            .status
            .success());
        runner.spawn_detached(OsStr::new("x"), &[]).unwrap();
    }

    /// Each forge has its own glyph colour, and anything else a neutral one.
    #[test]
    fn every_forge_gets_its_own_colour_and_bad_hex_is_ignored() {
        let theme = Theme::default();
        assert_eq!(host_icon("github.com", &theme).1, Color::Magenta);
        assert_eq!(host_icon("bitbucket.org", &theme).1, Color::Blue);
        assert_eq!(host_icon("gitlab.com", &theme).1, Color::Yellow);
        assert_eq!(host_icon("example.org", &theme).1, Color::DarkGray);
        assert_eq!(parse_hex("#abc"), None);
    }

    /// Lists herdr could not describe — not JSON, no array, rows with no id or
    /// no agent — contribute no rows rather than broken ones.
    #[test]
    fn malformed_herdr_lists_contribute_no_rows() {
        let theme = Theme::default();
        for body in [
            "not json",
            r#"{"result":{}}"#,
            r#"{"result":{"agents":[{"pane_id":"","agent":"claude"},{"pane_id":"p1","agent":""}],
                "workspaces":[{"workspace_id":""}]}}"#,
        ] {
            let runner = MockRunner::new().on("herdr", body);
            assert!(load_agents(&runner, &theme).is_empty(), "{body}");
            assert!(load_workspaces(&runner, &theme).is_empty(), "{body}");
        }
    }

    /// A linked worktree herdr reports without a HEAD is shown as detached.
    #[test]
    fn a_worktree_without_a_head_reads_as_detached() {
        let root = std::env::temp_dir().join(format!("swb-wt-nohead-{}", std::process::id()));
        let repo = root.join("gitlab.com/o/r");
        std::fs::create_dir_all(repo.join(".git/worktrees/x")).unwrap();
        let linked = root.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        let raw = format!(
            "worktree {}\0HEAD abc\0branch refs/heads/main\0\0worktree {}\0detached\0\0",
            repo.display(),
            linked.display()
        );
        let runner = MockRunner::new().on("worktree list", &raw);
        let entries = load_worktrees(
            &runner,
            &["gitlab.com/o/r".to_string()],
            &Theme::default(),
            root.to_str().unwrap(),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].secondary, "detached");
        std::fs::remove_dir_all(&root).ok();
    }
}
