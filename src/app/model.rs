//! Plain data: the workspace tree, sidebar rows, tabs, and UI interaction state.
use crate::{git, util};
use std::path::{Path, PathBuf};

pub struct Workspace {
    pub name: String,
    pub projects: Vec<Project>,
}

pub struct Project {
    pub path: PathBuf,
    pub name: String,
    pub worktrees: Option<Vec<git::Worktree>>,
}

pub fn is_default_branch(b: &str) -> bool {
    b == "main" || b == "master"
}

impl Project {
    /// A project for directory `path` (canonicalized); `None` if it isn't one.
    pub fn open(path: &Path) -> Option<Self> {
        let path = std::fs::canonicalize(path).ok().filter(|p| p.is_dir())?;
        Some(Project { name: util::file_name(&path), worktrees: git::worktrees(&path), path })
    }
    pub fn branch(&self) -> Option<&str> {
        self.worktrees.as_ref()?.iter().find(|w| w.main).map(|w| w.branch.as_str())
    }
    /// Worktrees listed under the project: all, except a main checkout on the default branch (that *is* the project row).
    pub fn listed(&self) -> impl Iterator<Item = (usize, &git::Worktree)> {
        self.worktrees.iter().flatten().enumerate().filter(|(_, w)| !(w.main && is_default_branch(&w.branch)))
    }
    pub fn has_linked(&self) -> bool {
        self.listed().next().is_some()
    }
    /// The project's own view plus one per linked worktree.
    pub fn view_paths(&self) -> Vec<PathBuf> {
        let mut v = vec![self.path.clone()];
        v.extend(self.worktrees.iter().flatten().filter(|w| !w.main).map(|w| w.path.clone()));
        v
    }
}

/// Fold keys in `App::folded`.
pub fn ws_key(name: &str) -> String {
    format!("w:{name}")
}
pub fn proj_key(p: &Path) -> String {
    format!("p:{}", p.display())
}

/// A sidebar row: indices into workspaces / projects / worktrees.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Ws(usize),
    Proj(usize, usize),
    Wt(usize, usize, usize),
}

impl Row {
    /// The workspace this row belongs to.
    pub fn ws(self) -> usize {
        match self {
            Row::Ws(w) | Row::Proj(w, _) | Row::Wt(w, _, _) => w,
        }
    }
    /// Tree depth: workspace 0, project 1, worktree 2.
    pub fn level(self) -> usize {
        match self {
            Row::Ws(_) => 0,
            Row::Proj(..) => 1,
            Row::Wt(..) => 2,
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

/// Something to remove from the sidebar.
#[derive(Clone)]
pub enum Close {
    Workspace(usize),
    Project(usize, usize),
    /// (project path, worktree path, force): `git worktree remove`, deletes the checkout.
    Worktree(PathBuf, PathBuf, bool),
}

/// A context-menu entry.
#[derive(Clone)]
pub enum Action {
    /// Add a project to this workspace (`None`: the selected one).
    AddProject(Option<usize>),
    NewWorkspace,
    RenameWorkspace(usize),
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
    AddProject(usize),
    NewWorkspace,
    NewWorktree(PathBuf),
    /// Yes/no before closing something that still has panes.
    Confirm(Close),
    /// (project path, worktree path): waiting for the dirty check before asking to delete.
    /// Becomes `Confirm` when it finishes; "yes" keys do nothing until then.
    CheckingWorktree(PathBuf, PathBuf),
    RenameWorkspace(usize),
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

/// Where a dragged sidebar row lands: insertion index before removal.
#[derive(Clone, Copy, PartialEq)]
pub enum Drop {
    Ws(usize),
    Proj(usize, usize),
}

/// A left-press on a workspace/project row: a click on release, or a reorder drag once it moves.
pub struct Drag {
    pub row: Row,
    pub(super) start_y: f32,
    pub active: bool,
    /// (target, row boundary the indicator line sits on, tree level of the line).
    pub target: Option<(Drop, usize, usize)>,
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
        let rows = [Row::Ws(0), Row::Proj(0, 0), Row::Wt(0, 0, 1), Row::Proj(0, 1), Row::Ws(1)];
        assert_eq!(subtree_end(&rows, 0), 4);
        assert_eq!(subtree_end(&rows, 1), 3);
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
