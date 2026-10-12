//! Sort menu and the visual rows separating repository groups.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;

use crate::data::{Entry, Kind, SortMode};

#[derive(Debug, PartialEq)]
pub(super) enum Row {
    Divider(String),
    Entry(usize),
}

pub(super) fn group_key(entry: &Entry, sort: SortMode) -> (u8, &str, &str) {
    match entry.kind {
        Kind::Agent => (0, "", ""),
        Kind::Workspace => (1, "", ""),
        Kind::Repo | Kind::Worktree => match &entry.repository {
            Some(repo) if !repo.host.is_empty() => (
                2,
                &repo.host,
                if sort == SortMode::Owner {
                    &repo.namespace
                } else {
                    ""
                },
            ),
            _ => (3, "", ""),
        },
    }
}

pub(super) fn rows(entries: &[Entry], filtered: &[usize], sort: SortMode, query: &str) -> Vec<Row> {
    let grouped = query.is_empty() && matches!(sort, SortMode::Provider | SortMode::Owner);
    let mut rows = Vec::new();
    let mut previous = None;
    for (rank, &index) in filtered.iter().enumerate() {
        if grouped {
            let key = group_key(&entries[index], sort);
            if previous != Some(key) {
                let label = match key {
                    (0, _, _) => "Agents".into(),
                    (1, _, _) => "Workspaces".into(),
                    (2, host, owner) if sort == SortMode::Owner => {
                        format!(
                            "{host}/{}",
                            if owner.is_empty() {
                                "(no owner)"
                            } else {
                                owner
                            }
                        )
                    }
                    (2, host, _) => host.into(),
                    _ => "Other".into(),
                };
                rows.push(Row::Divider(label));
                previous = Some(key);
            }
        }
        rows.push(Row::Entry(rank));
    }
    rows
}

#[derive(Default)]
pub(super) struct Menu {
    pub selected: usize,
    pub list_state: ListState,
    pub area: Rect,
    pub zones: Vec<(Rect, usize)>,
}

impl Menu {
    pub fn open(&mut self, current: SortMode) {
        self.selected = SortMode::ALL
            .iter()
            .position(|&sort| sort == current)
            .unwrap_or(0);
        self.list_state = ListState::default();
        self.zones.clear();
        self.area = Rect::default();
    }

    pub fn move_selection(&mut self, delta: i32) {
        self.selected =
            (self.selected as i32 + delta).rem_euclid(SortMode::ALL.len() as i32) as usize;
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Choice> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return (key.code == KeyCode::Char('c')).then_some(Choice::Quit);
        }
        if !key.modifiers.is_empty() {
            return None;
        }
        match key.code {
            KeyCode::Esc => return Some(Choice::Cancel),
            KeyCode::Enter => return Some(Choice::Apply(SortMode::ALL[self.selected])),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = SortMode::ALL.len() - 1,
            _ => {}
        }
        None
    }

    pub fn on_click(&mut self, at: Position) -> Option<Choice> {
        if !self.area.contains(at) {
            return Some(Choice::Cancel);
        }
        self.zones
            .iter()
            .find(|(zone, _)| zone.contains(at))
            .map(|(_, rank)| {
                self.selected = *rank;
                Choice::Apply(SortMode::ALL[*rank])
            })
    }
}

pub(super) enum Choice {
    Apply(SortMode),
    Cancel,
    Quit,
}
