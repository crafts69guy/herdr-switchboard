//! The registry of switchable entry sources.
//!
//! Each source knows its kind, whether it is turned on, and how to load its
//! entries. [`load_all`] folds the registry; [`kinds`] gives the canonical tab
//! order. Another source (tmux sessions, docker containers, MRU dirs…) is a
//! new [`Source`] impl plus one line in [`registry`] — nothing on the
//! load/metadata side has an exhaustive `match Kind` to update.
//!
//! The preview card ([`crate::preview::render`]) and the accept dispatch
//! ([`crate::action::dispatch`]) stay as compiler-checked `match`es in their own
//! modules on purpose: routing them through here would make `preview`/`action`
//! depend on this module and this module on them — a cycle — for no safety gain.
//! Adding a `Kind` variant already forces both of those matches at compile time.

use crate::data::{self, Config, Entry, Kind, Theme};
use crate::runner::CommandRunner;

/// What a source needs to produce its entries.
pub struct LoadCtx<'a> {
    pub runner: &'a dyn CommandRunner,
    pub theme: &'a Theme,
    pub root: &'a str,
}

/// One switchable source of entries.
pub trait Source {
    fn kind(&self) -> Kind;
    /// Whether this source is turned on, per the flat config.
    fn enabled(&self, cfg: &Config) -> bool;
    fn load(&self, ctx: &LoadCtx) -> Vec<Entry>;
}

struct Agents;
impl Source for Agents {
    fn kind(&self) -> Kind {
        Kind::Agent
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.bool("include_agents", true)
    }
    fn load(&self, ctx: &LoadCtx) -> Vec<Entry> {
        data::load_agents(ctx.runner, ctx.theme)
    }
}

struct Workspaces;
impl Source for Workspaces {
    fn kind(&self) -> Kind {
        Kind::Workspace
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.bool("include_workspaces", true)
    }
    fn load(&self, ctx: &LoadCtx) -> Vec<Entry> {
        data::load_workspaces(ctx.runner, ctx.theme)
    }
}

struct Repos;
impl Source for Repos {
    fn kind(&self) -> Kind {
        Kind::Repo
    }
    fn enabled(&self, _cfg: &Config) -> bool {
        true // repos are the reason the plugin exists; always listed
    }
    fn load(&self, ctx: &LoadCtx) -> Vec<Entry> {
        data::load_repos(ctx.runner, ctx.theme, ctx.root)
    }
}

struct Worktrees;
impl Source for Worktrees {
    fn kind(&self) -> Kind {
        Kind::Worktree
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.bool("include_worktrees", true)
    }
    fn load(&self, ctx: &LoadCtx) -> Vec<Entry> {
        data::load_worktrees(ctx.runner, ctx.theme, ctx.root)
    }
}

/// The sources in list/tab order: agents, workspaces, repos, worktrees.
pub fn registry() -> Vec<Box<dyn Source>> {
    vec![
        Box::new(Agents),
        Box::new(Workspaces),
        Box::new(Repos),
        Box::new(Worktrees),
    ]
}

/// Load every enabled source, in registry order.
pub fn load_all(cfg: &Config, ctx: &LoadCtx) -> Vec<Entry> {
    registry()
        .iter()
        .filter(|s| s.enabled(cfg))
        .flat_map(|s| s.load(ctx))
        .collect()
}

/// The kinds the registry defines, in order — the canonical tab order before it
/// is narrowed to the kinds actually present.
pub fn kinds() -> Vec<Kind> {
    registry().iter().map(|s| s.kind()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    const AGENTS: &str = r#"{"result":{"agents":[{"pane_id":"w1:p1","terminal_id":"t1","agent":"claude","agent_status":"idle","foreground_cwd":"/p"}]}}"#;
    const WORKSPACES: &str = r#"{"result":{"workspaces":[{"workspace_id":"w1","label":"work","number":1,"pane_count":1}]}}"#;
    const REPOS: &str = "github.com/o/a\ngithub.com/o/b\n";

    fn ctx<'a>(runner: &'a MockRunner, theme: &'a Theme) -> LoadCtx<'a> {
        LoadCtx {
            runner,
            theme,
            root: "/root",
        }
    }

    #[test]
    fn load_all_returns_sources_in_registry_order() {
        let runner = MockRunner::new()
            .on("herdr agent list", AGENTS)
            .on("herdr workspace list", WORKSPACES)
            .on("ghq list", REPOS);
        let theme = Theme::default();
        let e = load_all(&Config::default(), &ctx(&runner, &theme));
        let got: Vec<Kind> = e.iter().map(|x| x.kind).collect();
        assert_eq!(
            got,
            vec![Kind::Agent, Kind::Workspace, Kind::Repo, Kind::Repo]
        );
    }

    #[test]
    fn load_all_skips_a_disabled_source_without_querying_it() {
        let cfg = Config::from_pairs(&[
            ("include_agents", "false"),
            ("include_workspaces", "false"),
            ("include_worktrees", "false"),
        ]);
        let runner = MockRunner::new().on("ghq list", REPOS);
        let theme = Theme::default();
        let e = load_all(&cfg, &ctx(&runner, &theme));

        assert!(e.iter().all(|x| x.kind == Kind::Repo));
        // A disabled source must not even be queried.
        assert!(
            !runner
                .calls()
                .iter()
                .any(|c| c.contains(&"agent".to_string())),
            "include_agents=false must skip the agent query"
        );
        assert!(
            !runner
                .calls()
                .iter()
                .any(|c| c.first().is_some_and(|p| p == "git")),
            "include_worktrees=false must skip every git worktree query"
        );
    }

    #[test]
    fn kinds_are_the_registry_order() {
        assert_eq!(
            kinds(),
            vec![Kind::Agent, Kind::Workspace, Kind::Repo, Kind::Worktree]
        );
    }
}
