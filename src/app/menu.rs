//! Right-click context menus and the actions their items run.
use super::{item, proj_key, sep, ws_key, Action, App, Close, Menu, MenuItem, Row};
use crate::{config, platform};
use winit::event::KeyEvent;
use winit::keyboard::NamedKey;

impl App {
    /// Context menu for sidebar row `row` (or the empty area below the rows).
    pub(super) fn open_sidebar_menu(&mut self, row: Option<usize>, x: f32, y: f32) {
        let rows = self.rows();
        let fold = |app: &Self, key: String| item(if app.folded.contains(&key) { "Expand" } else { "Collapse" }, Action::Toggle(key));
        let items: Vec<MenuItem> = match row.and_then(|i| rows.get(i).copied()) {
            Some(Row::Ws(w)) => {
                let ws = &self.workspaces[w];
                let name = ws.name.clone();
                let mut v = vec![item("New project…", Action::AddProject(Some(name.clone()))), fold(self, ws_key(&name))];
                v.extend(path_items(&ws.path));
                v.extend([sep(), item("Close", Action::Delete(Close::Workspace(name)))]);
                v
            }
            Some(Row::Proj(w, p)) => {
                let pr = self.project(w, p);
                let mut v = Vec::new();
                if pr.worktrees.is_some() {
                    v.push(item("New worktree…", Action::NewWorktree(pr.path.clone())));
                }
                if pr.has_linked() {
                    v.push(fold(self, proj_key(&pr.path)));
                }
                // No separator on top when nothing comes before the path items.
                v.extend(path_items(&pr.path).into_iter().skip(v.is_empty() as usize));
                v.extend([sep(), item("Close", Action::Delete(Close::Project(pr.path.clone())))]);
                v
            }
            Some(Row::Wt(w, p, t)) => {
                let pr = self.project(w, p);
                let wt = &pr.worktrees.as_ref().expect("row has worktrees")[t];
                let mut v = vec![item("Open", Action::Open(wt.path.clone()))];
                v.extend(path_items(&wt.path));
                // Not while it's already being deleted: the row says "deleting…".
                if !wt.main && !self.removing.contains(&wt.path) {
                    v.extend([sep(), item("Delete worktree", Action::Delete(Close::Worktree(pr.path.clone(), wt.path.clone())))]);
                }
                v
            }
            Some(Row::New(..)) => return,
            None => vec![item("New workspace…", Action::NewWorkspace), item("Add project…", Action::AddProject(None))],
        };
        if let Some(i) = row {
            self.sel = i;
        }
        self.show_menu(items, x, y);
    }

    /// Open a menu at (x, y), kept inside the window.
    pub(super) fn show_menu(&mut self, items: Vec<MenuItem>, x: f32, y: f32) {
        let (w, h) = self.menu_size(&items);
        let x = x.min(self.w - w - self.px(4.0)).max(0.0);
        let y = y.min(self.h - h - self.px(4.0)).max(0.0);
        self.menu = Some(Menu { x, y, items, hover: None });
        self.dirty = true;
    }

    pub(super) fn menu_key(&mut self, k: &KeyEvent) {
        let Some(m) = self.menu.as_mut() else { return };
        let n = m.items.len();
        // Next non-separator item in direction `d`, wrapping.
        let step = |from: Option<usize>, d: isize| -> Option<usize> {
            let mut i = from.map_or(if d > 0 { -1 } else { n as isize }, |i| i as isize);
            for _ in 0..n {
                i = (i + d).rem_euclid(n as isize);
                if !matches!(m.items[i as usize].1, Action::Sep) {
                    return Some(i as usize);
                }
            }
            None
        };
        match crate::input::named(k) {
            Some(NamedKey::ArrowDown) => m.hover = step(m.hover, 1),
            Some(NamedKey::ArrowUp) => m.hover = step(m.hover, -1),
            Some(NamedKey::Enter) => {
                if let Some(i) = m.hover {
                    let a = m.items[i].1.clone();
                    self.menu = None;
                    self.run_action(a);
                }
            }
            Some(NamedKey::Escape) => self.menu = None,
            _ => {}
        }
    }

    pub(super) fn run_action(&mut self, a: Action) {
        self.dirty = true;
        match a {
            Action::AddProject(None) => self.prompt_add_project(None),
            Action::AddProject(Some(name)) => match self.ws_index(&name) {
                Some(w) => self.prompt_add_project(Some(w)),
                None => self.say(format!("workspace {name} no longer exists")),
            },
            Action::NewWorkspace => self.prompt_new_workspace(),
            Action::Toggle(key) => self.toggle_fold(key),
            Action::NewWorktree(p) => self.prompt_new_worktree(p),
            Action::Open(v) => self.open_view(v),
            Action::CopyPath(p) => {
                platform::clipboard_set(&p.display().to_string());
                self.say(format!("copied {}", config::tilde(&p)));
            }
            Action::Reveal(p) => platform::reveal(&p),
            Action::Copy(t) => {
                platform::clipboard_set(&t);
                self.say(format!("copied {t}"));
            }
            Action::OpenFile(p) => self.open_file(&p),
            Action::EditFile(p) => self.edit_in_terminal(&p),
            Action::Terminal(d) => {
                self.spawn_here(Some(&d), true);
            }
            Action::FileToggle(p) => {
                if let Some(i) = self.explorer().and_then(|e| e.entries.iter().position(|en| en.path == p)) {
                    self.files_toggle(i);
                }
            }
            Action::Delete(c) => self.request_close(c),
            Action::Sep => {}
        }
    }
}

/// "Copy path" / "Reveal" items for a folder, after a separator.
fn path_items(p: &std::path::Path) -> [MenuItem; 3] {
    [sep(), item("Copy path", Action::CopyPath(p.to_path_buf())), item(platform::REVEAL_LABEL, Action::Reveal(p.to_path_buf()))]
}
