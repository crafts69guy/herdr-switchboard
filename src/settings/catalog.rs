pub(super) enum Cycle {
    /// Step through a fixed ring. An unrecognised current value lands on the first
    /// entry, matching the `*)` fallback each `cycle()` case in the old bash form had.
    Ring(&'static [&'static str]),
    /// Free text, typed in place. Only `split_ratio` wants this.
    Prompt,
}

pub(super) struct Setting {
    /// The section this setting sits under; a new value starts a new heading,
    /// so the array's order is the display order (like the `?` cheatsheet).
    pub(super) group: &'static str,
    pub(super) key: &'static str,
    pub(super) default: &'static str,
    pub(super) hint: &'static str,
    pub(super) cycle: Cycle,
}

const BOOL: &[&str] = &["true", "false"];

/// The settings, in display order, grouped into sections. `write_setting` is
/// keyed by `key`, so the order here is free to read well.
pub(super) const SETTINGS: &[Setting] = &[
    Setting {
        group: "Open",
        key: "default_target",
        default: "workspace",
        hint: "where Enter opens a repo",
        cycle: Cycle::Ring(&["workspace", "tab", "split", "pane"]),
    },
    Setting {
        group: "Open",
        key: "split_direction",
        default: "right",
        hint: "split growth direction",
        cycle: Cycle::Ring(&["right", "down"]),
    },
    Setting {
        group: "Open",
        key: "split_ratio",
        default: "0.5",
        hint: "split size (0.1-0.9)",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Open",
        key: "label",
        default: "repo",
        hint: "workspace/tab label style",
        cycle: Cycle::Ring(&["repo", "owner-repo", "path"]),
    },
    Setting {
        group: "Sources",
        key: "include_agents",
        default: "true",
        hint: "list running agents in the switcher",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Sources",
        key: "include_workspaces",
        default: "true",
        hint: "list open workspaces in the switcher",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Sources",
        key: "include_worktrees",
        default: "true",
        hint: "list linked Git worktrees in the switcher",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Sources",
        key: "default_tab",
        default: "all",
        hint: "active tab at startup and after apply",
        cycle: Cycle::Ring(&["all", "agents", "workspaces", "repos", "worktrees"]),
    },
    Setting {
        group: "Sources",
        key: "sort",
        default: "recent",
        hint: "resting list order (recent/name/kind)",
        cycle: Cycle::Ring(&["recent", "name", "kind"]),
    },
    Setting {
        group: "Keys",
        key: "keymode",
        default: "normal",
        hint: "start mode: insert (type-to-filter) or normal (Vim)",
        cycle: Cycle::Ring(&["insert", "normal"]),
    },
    Setting {
        group: "Preview",
        key: "preview",
        default: "enabled",
        hint: "show the preview pane",
        cycle: Cycle::Ring(&["enabled", "disabled"]),
    },
    Setting {
        group: "Preview",
        key: "preview_position",
        default: "down",
        hint: "which side the preview sits on",
        cycle: Cycle::Ring(&["right", "down", "up", "left"]),
    },
    Setting {
        group: "Preview",
        key: "preview_size",
        default: "60%",
        hint: "preview share of the body",
        cycle: Cycle::Ring(&["40%", "50%", "60%", "70%", "80%"]),
    },
    Setting {
        group: "Preview",
        key: "preview_readme",
        default: "true",
        hint: "include README in the preview",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Appearance",
        key: "title_color",
        default: "peach",
        hint: "box title colour (theme slot or #hex)",
        cycle: Cycle::Ring(&["peach", "mauve", "teal", "blue", "accent"]),
    },
    Setting {
        group: "Appearance",
        key: "transparency",
        default: "transparent",
        hint: "all surface backgrounds",
        cycle: Cycle::Ring(&["transparent", "opaque"]),
    },
    Setting {
        group: "Clone",
        key: "clone_source",
        default: "clipboard",
        hint: "seed clone input from clipboard",
        cycle: Cycle::Ring(&["clipboard", "prompt"]),
    },
    Setting {
        group: "Clone",
        key: "open_after_clone",
        default: "true",
        hint: "open a repo right after cloning",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Updates",
        key: "update_check",
        default: "true",
        hint: "check GitHub daily for a newer version",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Notifications",
        key: "notifications",
        default: "true",
        hint: "show herdr notifications",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Notifications",
        key: "notification_position",
        default: "top-right",
        hint: "notification corner",
        cycle: Cycle::Ring(&["top-right", "top-left", "bottom-left", "bottom-right"]),
    },
    Setting {
        group: "Notifications",
        key: "notification_sound",
        default: "auto",
        hint: "toast sound: auto per-event, or force one",
        cycle: Cycle::Ring(&["auto", "none", "done", "request"]),
    },
    Setting {
        group: "Git",
        key: "base_branch",
        default: "",
        hint: "base for review branch (blank = auto-detect)",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Git",
        key: "all_files_warn",
        default: "1500",
        hint: "confirm all-files over N tracked files (0 = never)",
        cycle: Cycle::Ring(&["1500", "0", "500", "5000"]),
    },
    Setting {
        group: "Catalog",
        key: "history_limit",
        default: "5000",
        hint: "maximum imported command records",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Catalog",
        key: "command_sort",
        default: "frecency",
        hint: "command resting order",
        cycle: Cycle::Ring(&["frecency", "recent", "frequency", "alphabetical"]),
    },
    Setting {
        group: "Monitor",
        key: "refresh_interval_ms",
        default: "2000",
        hint: "listener refresh interval (minimum 250ms)",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Zen",
        key: "zen_width",
        default: "70",
        hint: "zen'd pane's share of the tab (20-95%)",
        cycle: Cycle::Ring(&["60", "70", "80", "90"]),
    },
    Setting {
        group: "Zen",
        key: "zen_scrim",
        default: "true",
        hint: "dim the zen gutters",
        cycle: Cycle::Ring(BOOL),
    },
    Setting {
        group: "Usage",
        key: "usage_warn_percent",
        default: "60",
        hint: "ungraded bar level that turns yellow",
        cycle: Cycle::Ring(&["50", "60", "70", "80"]),
    },
    Setting {
        group: "Usage",
        key: "usage_alert_percent",
        default: "85",
        hint: "ungraded bar level that turns red",
        cycle: Cycle::Ring(&["75", "85", "90", "95"]),
    },
    Setting {
        group: "Zen",
        key: "zen_scrim_color",
        default: "#11111b",
        hint: "gutter colour (#rrggbb; herdr paints it opaque)",
        cycle: Cycle::Prompt,
    },
    Setting {
        group: "Zen",
        key: "zen_chrome",
        default: "off",
        hint: "hide herdr chrome while zen'd (panes: borders; full: +tabs/sidebar)",
        cycle: Cycle::Ring(&["off", "panes", "full"]),
    },
];

/// The next value in a ring. An unknown current value restarts at the first.
pub(super) fn next_in(ring: &[&str], current: &str) -> String {
    let i = ring.iter().position(|v| *v == current);
    match i {
        Some(i) => ring[(i + 1) % ring.len()].to_string(),
        None => ring[0].to_string(),
    }
}
