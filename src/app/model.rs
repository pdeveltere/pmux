//! Plain data: the workspace tree, sidebar rows, tabs, and UI interaction state.
use crate::{git, util};
use std::path::{Path, PathBuf};

/// A workspace: one directory, and its git worktrees nested under it.
pub struct Workspace {
    pub path: PathBuf,
    pub name: String,
    pub worktrees: Option<Vec<git::Worktree>>,
}

pub fn is_default_branch(b: &str) -> bool {
    b == "main" || b == "master"
}

impl Workspace {
    /// A workspace for directory `path` (canonicalized); `None` if it isn't one.
    pub fn open(path: &Path) -> Option<Self> {
        let path = std::fs::canonicalize(path).ok().filter(|p| p.is_dir())?;
        Some(Workspace { name: util::file_name(&path), worktrees: git::worktrees(&path), path })
    }
    pub fn branch(&self) -> Option<&str> {
        self.worktrees.as_ref()?.iter().find(|w| w.main).map(|w| w.branch.as_str())
    }
    /// Worktrees listed under the workspace: all, except a main checkout on the default branch (that *is* the workspace row).
    pub fn listed(&self) -> impl Iterator<Item = (usize, &git::Worktree)> {
        self.worktrees.iter().flatten().enumerate().filter(|(_, w)| !(w.main && is_default_branch(&w.branch)))
    }
    pub fn has_linked(&self) -> bool {
        self.listed().next().is_some()
    }
    /// The workspace's own view plus one per linked worktree.
    pub fn view_paths(&self) -> Vec<PathBuf> {
        let mut v = vec![self.path.clone()];
        v.extend(self.worktrees.iter().flatten().filter(|w| !w.main).map(|w| w.path.clone()));
        v
    }
}

/// Fold key in `App::folded` of the workspace at `p`.
pub fn ws_key(p: &Path) -> String {
    format!("p:{}", p.display())
}

/// A sidebar row: indices into workspaces / worktrees.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Ws(usize),
    Wt(usize, usize),
}

impl Row {
    /// The workspace this row belongs to.
    pub fn ws(self) -> usize {
        match self {
            Row::Ws(w) | Row::Wt(w, _) => w,
        }
    }
    /// Tree depth: workspace 0, worktree 1.
    pub fn level(self) -> usize {
        match self {
            Row::Ws(_) => 0,
            Row::Wt(..) => 1,
        }
    }
}

/// End (exclusive) of the block of `rows[i]` and everything nested under it.
pub fn subtree_end(rows: &[Row], i: usize) -> usize {
    let lv = rows[i].level();
    rows[i + 1..].iter().position(|r| r.level() <= lv).map_or(rows.len(), |k| i + 1 + k)
}

/// One tab of a view: its panes, side by side.
#[derive(Default)]
pub struct Tab {
    pub panes: Vec<u32>,
    pub focus: usize,
}

/// A worktree's tabs.
#[derive(Default)]
pub struct View {
    pub tabs: Vec<Tab>,
    pub tab: usize,
}

impl View {
    pub fn cur(&self) -> Option<&Tab> {
        self.tabs.get(self.tab)
    }
    pub(super) fn cur_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.tab)
    }
    /// Show the tab holding `id` and focus it there.
    pub(super) fn reveal(&mut self, id: u32) {
        if let Some(t) = self.tabs.iter().position(|t| t.panes.contains(&id)) {
            self.tab = t;
            let tab = &mut self.tabs[t];
            tab.focus = tab.panes.iter().position(|x| *x == id).unwrap_or(0);
        }
    }
    /// Drop pane `id`; a tab whose last pane goes is removed too.
    pub(super) fn remove(&mut self, id: u32) {
        let Some(ti) = self.tabs.iter().position(|t| t.panes.contains(&id)) else { return };
        let t = &mut self.tabs[ti];
        t.panes.retain(|x| *x != id);
        t.focus = t.focus.min(t.panes.len().saturating_sub(1));
        if t.panes.is_empty() {
            self.tabs.remove(ti);
            if self.tab > ti || self.tab >= self.tabs.len() {
                self.tab = self.tab.saturating_sub(1);
            }
        }
    }
}

/// Something to remove from the sidebar. Named by identity (path), not by position: a menu or
/// y/n prompt can stay open while an agent's `pmux ws rm` / `ws add` shifts the list, and must
/// still act on what it was opened for (or on nothing).
#[derive(Clone)]
pub enum Close {
    Workspace(PathBuf),
    /// (workspace path, worktree path): `git worktree remove --force`, deletes the checkout.
    Worktree(PathBuf, PathBuf),
}

/// A `git worktree remove` in flight: what it removes, and who waits for the answer.
pub struct Removal {
    /// The workspace (repo) it belongs to.
    pub proj: PathBuf,
    pub wt: PathBuf,
    /// Display name ("worktree feat-x").
    pub name: String,
    /// `--force`: also when it has uncommitted changes.
    pub force: bool,
    /// A `pmux worktree rm` caller waiting for the result.
    pub reply: Option<std::sync::mpsc::Sender<crate::api::Reply>>,
    /// Set when pmux deletes it by itself because it's merged: then the branch goes too.
    pub auto: Option<super::merged::Auto>,
}

/// A context-menu entry.
#[derive(Clone)]
pub enum Action {
    /// Add a workspace (asks for its folder).
    AddWorkspace,
    Toggle(String),
    NewWorktree(PathBuf),
    Open(PathBuf),
    CopyPath(PathBuf),
    Reveal(PathBuf),
    /// Copy text to the clipboard as-is.
    Copy(String),
    /// Open a file in an editor tab.
    OpenFile(PathBuf),
    /// Open a file in `$EDITOR` in a new terminal tab.
    EditFile(PathBuf),
    /// New terminal tab with this cwd.
    Terminal(PathBuf),
    /// Expand/collapse an explorer folder.
    FileToggle(PathBuf),
    Delete(Close),
    Sep,
}

pub type MenuItem = (String, Action);

pub fn item(label: &str, a: Action) -> MenuItem {
    (label.into(), a)
}

pub fn sep() -> MenuItem {
    (String::new(), Action::Sep)
}

pub struct Menu {
    pub x: f32,
    pub y: f32,
    pub items: Vec<MenuItem>,
    pub hover: Option<usize>,
}

pub enum PromptKind {
    /// Add a workspace: the folder.
    AddWorkspace,
    NewWorktree(PathBuf),
    /// Yes/no before closing something that still has panes.
    Confirm(Close),
}

pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub input: String,
}

impl Prompt {
    pub(super) fn new(kind: PromptKind, label: impl Into<String>, input: &str) -> Self {
        Prompt { kind, label: label.into(), input: input.into() }
    }
}

/// A left-press on a workspace row: a click on release, or a reorder drag once it moves.
pub struct Drag {
    pub row: Row,
    pub(super) start_y: f32,
    pub active: bool,
    /// (workspace insertion index before removal, row boundary the indicator line sits on).
    pub target: Option<(usize, usize)>,
}

/// A part of the tab bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabHit {
    Tab(usize),
    /// The tab's × button.
    Close(usize),
    /// The + (new tab) button.
    New,
}

impl TabHit {
    /// The tab this is on (its body or its ×).
    pub fn tab(self) -> Option<usize> {
        match self {
            TabHit::Tab(i) | TabHit::Close(i) => Some(i),
            TabHit::New => None,
        }
    }
}

/// A resizable panel edge.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Edge {
    Sidebar,
    Files,
    /// The line between the workspace tree and the agents panel.
    Agents,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtree_and_view_remove() {
        let rows = [Row::Ws(0), Row::Wt(0, 1), Row::Wt(0, 2), Row::Ws(1), Row::Ws(2)];
        assert_eq!(subtree_end(&rows, 0), 3);
        assert_eq!(subtree_end(&rows, 1), 2);
        assert_eq!(subtree_end(&rows, 3), 4);
        assert_eq!(subtree_end(&rows, 4), 5);

        let mut v = View { tabs: vec![Tab { panes: vec![1], focus: 0 }, Tab { panes: vec![2, 3], focus: 1 }], tab: 1 };
        v.remove(3);
        assert_eq!((v.tab, v.tabs[1].focus), (1, 0));
        v.remove(1);
        assert_eq!((v.tabs.len(), v.tab), (1, 0));
        v.reveal(2);
        assert_eq!(v.cur().unwrap().panes, vec![2]);
    }
}
