//! The project catalog: one cohesive loader for every entry kind.
//!
//! `ProjectCatalog` keeps inclusion and ordering policy explicit while leaving
//! the individual entry parsers in `data`.
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

pub struct ProjectCatalog<'a> {
    config: &'a Config,
    context: LoadCtx<'a>,
}

impl<'a> ProjectCatalog<'a> {
    pub fn new(config: &'a Config, context: LoadCtx<'a>) -> Self {
        Self { config, context }
    }

    pub fn load(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        if self.config.projects.include_agents {
            entries.extend(data::load_agents(self.context.runner, self.context.theme));
        }
        if self.config.projects.include_workspaces {
            entries.extend(data::load_workspaces(
                self.context.runner,
                self.context.theme,
            ));
        }
        // Repositories are the product's anchor and are always present.
        entries.extend(data::load_repos(
            self.context.runner,
            self.context.theme,
            self.context.root,
        ));
        if self.config.projects.include_worktrees {
            entries.extend(data::load_worktrees(
                self.context.runner,
                self.context.theme,
                self.context.root,
            ));
        }
        entries
    }
}

/// Load every enabled entry kind, in catalog order.
pub fn load_all(cfg: &Config, ctx: &LoadCtx) -> Vec<Entry> {
    ProjectCatalog::new(
        cfg,
        LoadCtx {
            runner: ctx.runner,
            theme: ctx.theme,
            root: ctx.root,
        },
    )
    .load()
}

/// The catalog's canonical tab order before it is narrowed to the kinds
/// actually present.
pub fn kinds() -> Vec<Kind> {
    vec![Kind::Agent, Kind::Workspace, Kind::Repo, Kind::Worktree]
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
    fn load_all_returns_entries_in_catalog_order() {
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
        let mut cfg = Config::default();
        cfg.projects.include_agents = false;
        cfg.projects.include_workspaces = false;
        cfg.projects.include_worktrees = false;
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
    fn kinds_are_in_catalog_order() {
        assert_eq!(
            kinds(),
            vec![Kind::Agent, Kind::Workspace, Kind::Repo, Kind::Worktree]
        );
    }
}
