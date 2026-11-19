//! Agents panel (bottom of the sidebar): every agent pane, grouped Needs you / Done / Working / Idle.
use super::{App, State, Tree, R};
use std::time::{Duration, Instant};

/// Default share of the sidebar's height the agents panel takes.
pub const AGENTS_SHARE: f32 = 0.4;
/// How close to the panel's top edge the mouse grabs it.
const EDGE_GRAB: f32 = 3.0;

/// A section of the agents panel, most urgent first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    NeedsYou,
    Done,
    Working,
    Idle,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::NeedsYou, Group::Done, Group::Working, Group::Idle];

    pub fn label(self) -> &'static str {
        match self {
            Group::NeedsYou => "Needs you",
            Group::Done => "Done",
            Group::Working => "Working",
            Group::Idle => "Idle",
        }
    }

    fn of(s: State) -> Self {
        match s {
            State::Waiting | State::Error => Group::NeedsYou,
            State::Done => Group::Done,
            State::Working => Group::Working,
            State::Idle => Group::Idle,
        }
    }

    fn fold_key(self) -> String {
        format!("a:{}", self.label())
    }
}

/// A row of the agents panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentRow {
    /// Section header: group, how many agents it has.
    Header(Group, usize),
    Agent(u32),
    /// Blank row between two sections.
    Gap,
}

impl App {
    /// The group a pane is listed in. A done agent you're looking at stays in Done until you leave it.
    pub fn agent_group(&self, id: u32) -> Option<Group> {
        let s = self.panes.get(&id)?.state()?;
        let held = self.held == Some(id) && s == State::Idle;
        Some(if held { Group::Done } else { Group::of(s) })
    }

    /// Panel rows: each non-empty group's header, then its agents (most recently changed first) unless
    /// folded, with a blank row between groups.
    pub fn agent_rows(&self) -> Vec<AgentRow> {
        let mut by: Vec<(Group, Instant, u32)> =
            self.panes.values().filter_map(|p| Some((self.agent_group(p.id)?, p.since(), p.id))).collect();
        by.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        let mut rows = Vec::new();
        for g in Group::ALL {
            let ids: Vec<u32> = by.iter().filter(|e| e.0 == g).map(|e| e.2).collect();
            if ids.is_empty() {
                continue;
            }
            if !rows.is_empty() {
                rows.push(AgentRow::Gap);
            }
            rows.push(AgentRow::Header(g, ids.len()));
            if !self.folded.contains(&g.fold_key()) {
                rows.extend(ids.into_iter().map(AgentRow::Agent));
            }
        }
        rows
    }

    pub fn agent_group_folded(&self, g: Group) -> bool {
        self.folded.contains(&g.fold_key())
    }

    /// Is pane `id` on screen (in the current view's shown tab)?
    pub(super) fn shown(&self, id: u32) -> bool {
        self.current.as_ref().is_some_and(|v| self.tab_panes(v).contains(&id))
    }

    /// Open pane `id`'s worktree, show its tab and focus it.
    pub(super) fn show_pane(&mut self, id: u32) {
        let Some(v) = self.panes.get(&id).map(|p| p.view.clone()) else { return };
        self.open_view(v.clone());
        if let Some(view) = self.views.get_mut(&v) {
            view.reveal(id);
        }
        self.focus_panes();
    }

    // ---- geometry ----

    /// The agents panel: the bottom of the sidebar, below the workspace tree.
    pub fn agents_rect(&self) -> R {
        let c = self.sidebar_content();
        if !self.show_agents {
            return R { y: c.y + c.h, h: 0.0, ..c };
        }
        // Each side keeps at least its header and one row.
        let min = self.px(super::PANEL_HEADER_H + super::ROW_H);
        let h = (c.h * self.agents_share).round().clamp(min.min(c.h), (c.h - min).max(min.min(c.h)));
        R { y: c.y + c.h - h, h, ..c }
    }

    /// Is (x, y) on the line between the workspace tree and the agents panel?
    pub(super) fn agents_edge_at(&self, x: f32, y: f32) -> bool {
        let a = self.agents_rect();
        a.h > 0.0 && x >= a.x && x < a.x + a.w && (y - a.y).abs() <= self.px(EDGE_GRAB)
    }

    /// Drag the panel's top edge to screen y.
    pub(super) fn resize_agents(&mut self, y: f32) {
        let c = self.sidebar_content();
        if c.h > 0.0 {
            self.agents_share = ((c.y + c.h - y) / c.h).clamp(0.0, 1.0);
            let len = self.agent_rows().len();
            self.agents_top = self.agents_tree(len).top;
        }
    }

    pub fn agents_header(&self) -> R {
        self.header_of(self.agents_rect())
    }

    /// The panel's rows, scrolled by `agents_top`.
    pub fn agents_tree(&self, len: usize) -> Tree {
        let t = self.make_tree(self.below_header(self.agents_rect()), 0, len);
        let top = self.agents_top.min(len.saturating_sub(t.capacity()));
        Tree { top, ..t }
    }

    /// The clickable row at screen y (gaps aren't).
    pub(super) fn agent_row_at(&self, y: f32) -> Option<usize> {
        let rows = self.agent_rows();
        self.agents_tree(rows.len()).index_at(y).filter(|&i| rows[i] != AgentRow::Gap)
    }

    // ---- mouse ----

    /// Left click in the agents panel: a header folds its group, an agent row jumps to the pane.
    pub(super) fn agents_click(&mut self, y: f32) {
        let rows = self.agent_rows();
        match self.agent_row_at(y).map(|i| rows[i]) {
            Some(AgentRow::Header(g, _)) => self.toggle_fold(g.fold_key()),
            Some(AgentRow::Agent(id)) => self.show_pane(id),
            Some(AgentRow::Gap) | None => {}
        }
    }

    pub(super) fn agents_scroll(&mut self, n: i32) {
        let len = self.agent_rows().len();
        let cap = self.agents_tree(len).capacity();
        self.agents_top = (self.agents_top as i32 - n).clamp(0, len.saturating_sub(cap) as i32) as usize;
    }

    /// When an agent row's "how long" text changes next.
    pub(super) fn agents_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        self.panes.values().filter(|p| p.state().is_some()).map(|p| next_tick(p.since(), now)).min()
    }
}

/// "<1m", "5m", "3h", "2d": how long ago `since` was.
pub fn ago(since: Instant) -> String {
    let m = since.elapsed().as_secs() / 60;
    match m {
        0 => "<1m".into(),
        1..=59 => format!("{m}m"),
        60..=1439 => format!("{}h", m / 60),
        _ => format!("{}d", m / 1440),
    }
}

/// The next instant `ago(since)` reads differently.
fn next_tick(since: Instant, now: Instant) -> Instant {
    let e = now.saturating_duration_since(since).as_secs();
    let unit = match e {
        0..=3599 => 60,
        3600..=86399 => 3600,
        _ => 86400,
    };
    since + Duration::from_secs((e / unit + 1) * unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_and_ticks() {
        let now = Instant::now();
        let s = now - Duration::from_secs(125);
        assert_eq!(next_tick(s, now), s + Duration::from_secs(180));
        let s = now - Duration::from_secs(7300);
        assert_eq!(next_tick(s, now), s + Duration::from_secs(10800));
        assert_eq!(ago(now - Duration::from_secs(30)), "<1m");
        assert_eq!(ago(now - Duration::from_secs(600)), "10m");
        assert_eq!(ago(now - Duration::from_secs(3 * 3600)), "3h");
    }
}
