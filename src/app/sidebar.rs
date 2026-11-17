//! Sidebar: the workspace → project → worktree tree, its keys, reordering, adding and closing.
use super::{proj_key, subtree_end, ws_key, App, Close, Drop, Focus, Project, Prompt, PromptKind, Row, Workspace};
use crate::{config, git, input, util};
use std::path::{Path, PathBuf};
use winit::event::KeyEvent;
use winit::keyboard::NamedKey;

impl App {
    /// The visible sidebar rows, top to bottom.
    pub fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        for (wi, ws) in self.workspaces.iter().enumerate() {
            out.push(Row::Ws(wi));
            if self.folded.contains(&ws_key(&ws.name)) {
                continue;
            }
            for (pi, p) in ws.projects.iter().enumerate() {
                out.push(Row::Proj(wi, pi));
                if !self.folded.contains(&proj_key(&p.path)) {
                    out.extend(p.listed().map(|(ti, _)| Row::Wt(wi, pi, ti)));
                }
            }
        }
        out
    }

    pub(super) fn project(&self, w: usize, p: usize) -> &Project {
        &self.workspaces[w].projects[p]
    }

    /// The view a row opens; workspaces have none.
    pub fn row_view(&self, r: Row) -> Option<PathBuf> {
        match r {
            Row::Proj(wi, pi) => Some(self.project(wi, pi).path.clone()),
            Row::Wt(wi, pi, ti) => Some(self.project(wi, pi).worktrees.as_ref()?[ti].path.clone()),
            Row::Ws(_) => None,
        }
    }

    /// The fold key of a row that can fold.
    fn fold_key(&self, row: Row) -> Option<String> {
        match row {
            Row::Ws(w) => Some(ws_key(&self.workspaces[w].name)),
            Row::Proj(w, p) if self.project(w, p).has_linked() => Some(proj_key(&self.project(w, p).path)),
            _ => None,
        }
    }

    fn row_valid(&self, row: Row) -> bool {
        match row {
            Row::Ws(w) => w < self.workspaces.len(),
            Row::Proj(w, p) => self.workspaces.get(w).is_some_and(|ws| p < ws.projects.len()),
            Row::Wt(w, p, t) => self.workspaces.get(w).and_then(|ws| ws.projects.get(p)).and_then(|pr| pr.worktrees.as_ref()).is_some_and(|v| t < v.len()),
        }
    }

    fn select_row(&mut self, row: Row) {
        if let Some(i) = self.rows().iter().position(|r| *r == row) {
            self.sel = i;
        }
    }

    /// The workspace of the selected row.
    fn selected_ws(&self) -> Option<usize> {
        self.rows().get(self.sel).map(|r| r.ws())
    }

    /// The workspace holding a view.
    pub(super) fn ws_of(&self, view: &Path) -> Option<usize> {
        let name = self.names(view).0;
        self.workspaces.iter().position(|w| w.name == name)
    }

    /// Click (or release without a drag): open the row's view, or fold a workspace.
    pub(super) fn click_row(&mut self, row: Row) {
        if !self.row_valid(row) {
            return;
        }
        match self.row_view(row) {
            Some(v) => self.open_view(v),
            None => {
                if let Some(k) = self.fold_key(row) {
                    self.toggle_fold(k);
                }
                self.focus = Focus::Sidebar;
            }
        }
    }

    pub(super) fn sidebar_key(&mut self, k: &KeyEvent, ch: Option<char>) {
        let rows = self.rows();
        if rows.is_empty() {
            match ch {
                Some('W') => self.prompt_new_workspace(),
                Some('q') => self.quit = true,
                _ => {}
            }
            return;
        }
        self.sel = self.sel.min(rows.len() - 1);
        let row = rows[self.sel];
        let fold_key = self.fold_key(row);
        let folded = fold_key.as_ref().is_some_and(|f| self.folded.contains(f));
        let named = input::named(k);
        let is = |n: NamedKey, c: char| named == Some(n) || ch == Some(c);
        if is(NamedKey::ArrowDown, 'j') {
            self.sel = (self.sel + 1).min(rows.len() - 1);
        } else if is(NamedKey::ArrowUp, 'k') {
            self.sel = self.sel.saturating_sub(1);
        } else if is(NamedKey::ArrowRight, 'l') && folded {
            self.toggle_fold(fold_key.unwrap());
        } else if is(NamedKey::ArrowRight, 'l') || named == Some(NamedKey::Enter) {
            // Enter opens, or folds/unfolds a workspace; → only unfolds (handled above).
            match self.row_view(row) {
                Some(v) => self.open_view(v),
                None => {
                    if let Some(f) = fold_key.filter(|_| named == Some(NamedKey::Enter)) {
                        self.toggle_fold(f);
                    }
                }
            }
        } else if is(NamedKey::ArrowLeft, 'h') {
            // ← folds, or goes to the parent row.
            match (row, fold_key) {
                (_, Some(f)) if !folded => self.toggle_fold(f),
                (Row::Proj(w, _), _) => self.select_row(Row::Ws(w)),
                (Row::Wt(w, p, _), _) => self.select_row(Row::Proj(w, p)),
                _ => {}
            }
        } else if matches!(named, Some(NamedKey::Escape | NamedKey::Tab)) {
            if self.focused().is_some() {
                self.focus = Focus::Panes;
            }
        } else {
            match (ch, row) {
                (Some('a'), _) => self.prompt_add_project(Some(row.ws())),
                (Some('W'), _) => self.prompt_new_workspace(),
                (Some('w'), Row::Proj(w, p) | Row::Wt(w, p, _)) if self.project(w, p).worktrees.is_some() => {
                    self.prompt_new_worktree(self.project(w, p).path.clone())
                }
                (Some('w'), _) => self.say("not a git project"),
                (Some('x'), Row::Ws(w)) => self.request_close(Close::Workspace(w)),
                (Some('x'), Row::Proj(w, p)) => self.request_close(Close::Project(w, p)),
                (Some('x'), Row::Wt(..)) => self.say("x closes a project or workspace; D removes a worktree"),
                (Some('D'), Row::Wt(w, p, t)) => self.delete_worktree(w, p, t),
                (Some('q'), _) => self.quit = true,
                _ => {}
            }
        }
    }

    // ---- reorder by drag ----

    /// Drop target for dragging `row` with the mouse at screen y: workspaces reorder among themselves,
    /// projects reorder within or move across workspaces.
    pub(super) fn drop_target(&self, row: Row, y: f32) -> Option<(Drop, usize, usize)> {
        let rows = self.rows();
        let n = rows.len();
        if n == 0 {
            return None;
        }
        let pos = self.sidebar_tree(n).pos_at(y);
        let idx = (pos.floor().max(0.0) as usize).min(n - 1);
        // Row block [start, end): the row plus everything nested under it.
        let block = |start: usize| (start, subtree_end(&rows, start));
        let before = |(s, e): (usize, usize)| pos < (s + e) as f32 / 2.0;
        match row {
            Row::Ws(_) => {
                let w = rows[idx].ws();
                let b = block(rows.iter().position(|r| *r == Row::Ws(w))?);
                Some(if before(b) { (Drop::Ws(w), b.0, 0) } else { (Drop::Ws(w + 1), b.1, 0) })
            }
            Row::Proj(..) => match rows[idx] {
                // Upper half of a workspace row: end of the workspace above; lower half: top of this one.
                Row::Ws(w) if w > 0 && pos < idx as f32 + 0.5 => Some((Drop::Proj(w - 1, self.workspaces[w - 1].projects.len()), idx, 1)),
                Row::Ws(w) => Some((Drop::Proj(w, 0), idx + 1, 1)),
                Row::Proj(w, p) | Row::Wt(w, p, _) => {
                    let b = block(rows.iter().position(|r| *r == Row::Proj(w, p))?);
                    Some(if before(b) { (Drop::Proj(w, p), b.0, 1) } else { (Drop::Proj(w, p + 1), b.1, 1) })
                }
            },
            Row::Wt(..) => None,
        }
    }

    pub(super) fn drop_row(&mut self, row: Row, to: Drop) {
        if !self.row_valid(row) {
            return;
        }
        let moved = match (row, to) {
            (Row::Ws(from), Drop::Ws(to)) if to <= self.workspaces.len() => {
                let ws = self.workspaces.remove(from);
                let to = if to > from { to - 1 } else { to };
                self.workspaces.insert(to, ws);
                Row::Ws(to)
            }
            (Row::Proj(fw, fp), Drop::Proj(tw, tp)) if tw < self.workspaces.len() && tp <= self.workspaces[tw].projects.len() => {
                let path = &self.project(fw, fp).path;
                if tw != fw && self.workspaces[tw].projects.iter().any(|p| &p.path == path) {
                    return self.say(format!("already in workspace {}", self.workspaces[tw].name));
                }
                let pr = self.workspaces[fw].projects.remove(fp);
                let tp = if tw == fw && tp > fp { tp - 1 } else { tp };
                self.workspaces[tw].projects.insert(tp, pr);
                Row::Proj(tw, tp)
            }
            _ => return,
        };
        self.select_row(moved);
        self.save();
        self.dirty = true;
    }

    // ---- adding ----

    pub(super) fn prompt_new_workspace(&mut self) {
        self.prompt = Some(Prompt::new(PromptKind::NewWorkspace, "New workspace", ""));
    }

    pub(super) fn prompt_new_worktree(&mut self, project: PathBuf) {
        self.prompt = Some(Prompt::new(PromptKind::NewWorktree(project), "New worktree branch", ""));
    }

    pub(super) fn prompt_rename_workspace(&mut self, w: usize) {
        let name = self.workspaces[w].name.clone();
        self.prompt = Some(Prompt::new(PromptKind::RenameWorkspace(w), "Rename workspace", &name));
    }

    /// Add a project to workspace `w` (default: the selected row's workspace, else the last one).
    pub(super) fn prompt_add_project(&mut self, w: Option<usize>) {
        let w = match w.or_else(|| self.selected_ws()).or(self.workspaces.len().checked_sub(1)) {
            Some(w) => w,
            None => {
                // No workspace yet: make one so the project has a home.
                self.workspaces.push(Workspace { name: "default".into(), projects: vec![] });
                self.save();
                0
            }
        };
        let label = format!("Add project to {}", self.workspaces[w].name);
        self.prompt = Some(Prompt::new(PromptKind::AddProject(w), label, "~/"));
    }

    /// A workspace name that's not empty and not taken (except by workspace `except`).
    fn check_ws_name<'a>(&self, name: &'a str, except: Option<usize>) -> Result<&'a str, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("workspace name is empty".into());
        }
        if self.workspaces.iter().enumerate().any(|(i, ws)| Some(i) != except && ws.name == name) {
            return Err(format!("workspace {name} already exists"));
        }
        Ok(name)
    }

    pub(super) fn add_workspace(&mut self, name: &str) -> Result<(), String> {
        let name = self.check_ws_name(name, None)?;
        self.workspaces.push(Workspace { name: name.into(), projects: vec![] });
        self.sel = self.rows().len() - 1;
        self.save();
        self.dirty = true;
        Ok(())
    }

    pub(super) fn rename_workspace(&mut self, w: usize, name: &str) -> Result<(), String> {
        let name = self.check_ws_name(name, Some(w))?;
        let old = std::mem::replace(&mut self.workspaces[w].name, name.into());
        if self.folded.remove(&ws_key(&old)) {
            self.folded.insert(ws_key(name));
        }
        self.save();
        Ok(())
    }

    pub(super) fn add_project(&mut self, w: usize, dir: &str) -> Result<String, String> {
        let Some(proj) = Project::open(&config::expand(dir.trim())) else { return Err(format!("not a directory: {dir}")) };
        if w >= self.workspaces.len() {
            return Err("no such workspace".into());
        }
        if let Some(ws) = self.workspaces.iter().find(|ws| ws.projects.iter().any(|p| p.path == proj.path)) {
            return Err(format!("{} is already in workspace {}", proj.name, ws.name));
        }
        let name = proj.name.clone();
        self.workspaces[w].projects.push(proj);
        self.folded.remove(&ws_key(&self.workspaces[w].name));
        self.select_row(Row::Proj(w, self.workspaces[w].projects.len() - 1));
        self.save();
        self.rewatch();
        self.dirty = true;
        Ok(name)
    }

    /// Create a worktree for a new or existing branch and open it.
    pub(super) fn add_worktree(&mut self, project: &Path, branch: &str) {
        match git::add(project, branch) {
            Ok(dest) => {
                self.reload_git();
                self.open_view(dest);
            }
            Err(e) => self.say(e),
        }
    }

    // ---- closing (workspaces / projects: sidebar only, files are never touched) ----

    /// (display name, views it covers, panes open in them).
    pub(super) fn close_info(&self, c: &Close) -> (String, Vec<PathBuf>, usize) {
        let (name, paths) = match c {
            Close::Workspace(w) => {
                let ws = &self.workspaces[*w];
                (format!("workspace {}", ws.name), ws.projects.iter().flat_map(Project::view_paths).collect())
            }
            Close::Project(w, p) => {
                let pr = self.project(*w, *p);
                (format!("project {}", pr.name), pr.view_paths())
            }
            Close::Worktree(_, wt, _) => (format!("worktree {}", self.names(wt).2), vec![wt.clone()]),
        };
        let n = paths.iter().map(|v| self.panes_in(v).len()).sum();
        (name, paths, n)
    }

    /// Close right away if nothing runs inside, otherwise ask first.
    pub(super) fn request_close(&mut self, c: Close) {
        let (name, _, n) = self.close_info(&c);
        let (c, label) = match c {
            Close::Worktree(proj, wt, _) => {
                // Deletes files on disk: always ask. Say what else goes with it.
                let dirty = git::dirty(&wt);
                let panes = if n > 0 { format!(" and close its {}", util::plural(n, "pane")) } else { String::new() };
                let lost = if dirty { " It has uncommitted changes, which will be lost." } else { "" };
                (Close::Worktree(proj, wt, dirty), format!("Delete {name}{panes}? Removes its folder.{lost} y/n"))
            }
            c if n == 0 => return self.close(c),
            c @ Close::Project(..) => (c, format!("Close {name} and its {}? Files stay on disk. y/n", util::plural(n, "pane"))),
            c => (c, format!("Delete {name} and close its {}? Files stay on disk. y/n", util::plural(n, "pane"))),
        };
        self.prompt = Some(Prompt::new(PromptKind::Confirm(c), label, ""));
    }

    fn delete_worktree(&mut self, w: usize, p: usize, t: usize) {
        let pr = self.project(w, p);
        let wt = &pr.worktrees.as_ref().expect("row has worktrees")[t];
        if wt.main {
            return self.say("can't delete the main checkout");
        }
        self.request_close(Close::Worktree(pr.path.clone(), wt.path.clone(), false));
    }

    pub(super) fn close(&mut self, c: Close) {
        let (name, paths, _) = self.close_info(&c);
        let ids: Vec<u32> = paths.iter().flat_map(|v| self.panes_in(v)).collect();
        self.kill_panes(ids);
        if self.current.as_ref().is_some_and(|cur| paths.contains(cur)) {
            self.current = None;
            self.focus = Focus::Sidebar;
            for v in &paths {
                self.files.remove(v);
            }
            self.files_rewatch();
        }
        match c {
            Close::Worktree(proj, wt, force) => {
                self.say(match git::remove(&proj, &wt, force) {
                    Ok(()) => format!("deleted {name}"),
                    Err(e) => format!("couldn't delete {name}: {e}"),
                });
                self.reload_git();
            }
            Close::Workspace(w) => {
                let ws = self.workspaces.remove(w);
                self.folded.remove(&ws_key(&ws.name));
                self.closed_tree(format!("deleted {name} from pmux"));
            }
            Close::Project(w, p) => {
                let pr = self.workspaces[w].projects.remove(p);
                self.folded.remove(&proj_key(&pr.path));
                self.closed_tree(format!("closed {name}"));
            }
        }
        self.sel = self.sel.min(self.rows().len().saturating_sub(1));
        self.dirty = true;
    }

    /// After removing a workspace or project from the tree.
    fn closed_tree(&mut self, msg: String) {
        self.save();
        self.rewatch();
        self.say(msg);
    }
}
