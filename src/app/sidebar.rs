//! Sidebar: the workspace → worktree tree, its keys, reordering, adding and closing.
use super::{subtree_end, ws_key, App, Close, Ev, Focus, Prompt, PromptKind, Removal, Row, Workspace};
use crate::api::Reply;
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
            if !self.folded.contains(&ws_key(&ws.path)) {
                out.extend(ws.listed().map(|(ti, _)| Row::Wt(wi, ti)));
            }
        }
        out
    }

    /// The view a row opens.
    pub fn row_view(&self, r: Row) -> Option<PathBuf> {
        match r {
            Row::Ws(w) => Some(self.workspaces[w].path.clone()),
            Row::Wt(w, t) => Some(self.workspaces[w].worktrees.as_ref()?[t].path.clone()),
        }
    }

    /// The fold key of a row that can fold: a workspace with worktrees listed under it.
    fn fold_key(&self, row: Row) -> Option<String> {
        match row {
            Row::Ws(w) if self.workspaces[w].has_linked() => Some(ws_key(&self.workspaces[w].path)),
            _ => None,
        }
    }

    fn row_valid(&self, row: Row) -> bool {
        match row {
            Row::Ws(w) => w < self.workspaces.len(),
            Row::Wt(w, t) => self.workspaces.get(w).and_then(|ws| ws.worktrees.as_ref()).is_some_and(|v| t < v.len()),
        }
    }

    fn select_row(&mut self, row: Row) {
        if let Some(i) = self.rows().iter().position(|r| *r == row) {
            self.sel = i;
        }
    }

    /// The workspace at `path`.
    pub(super) fn ws_index(&self, path: &Path) -> Option<usize> {
        self.workspaces.iter().position(|w| w.path == path)
    }

    /// The workspace `t` names: its folder (`~/` allowed) or its name.
    pub(super) fn find_workspace(&self, t: &str) -> Option<PathBuf> {
        let path = std::fs::canonicalize(config::expand(t)).ok();
        self.workspaces.iter().find(|w| Some(&w.path) == path.as_ref() || w.name == t).map(|w| w.path.clone())
    }

    /// Click (or release without a drag): open the row's view.
    pub(super) fn click_row(&mut self, row: Row) {
        if let Some(v) = self.row_valid(row).then(|| self.row_view(row)).flatten() {
            self.open_view(v);
        }
    }

    pub(super) fn sidebar_key(&mut self, k: &KeyEvent, ch: Option<char>) {
        let rows = self.rows();
        if rows.is_empty() {
            match ch {
                Some('a' | 'W') => self.prompt_add_workspace(),
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
            if let Some(v) = self.row_view(row) {
                self.open_view(v);
            }
        } else if is(NamedKey::ArrowLeft, 'h') {
            // ← folds, or goes to the parent row.
            match (row, fold_key) {
                (_, Some(f)) if !folded => self.toggle_fold(f),
                (Row::Wt(w, _), _) => self.select_row(Row::Ws(w)),
                _ => {}
            }
        } else if matches!(named, Some(NamedKey::Escape | NamedKey::Tab)) {
            if self.focused().is_some() {
                self.focus = Focus::Panes;
            }
        } else {
            let ws = &self.workspaces[row.ws()];
            match (ch, row) {
                (Some('a' | 'W'), _) => self.prompt_add_workspace(),
                (Some('w'), _) if ws.worktrees.is_some() => self.prompt_new_worktree(ws.path.clone()),
                (Some('w'), _) => self.say("not a git repo"),
                (Some('x'), Row::Wt(..)) => self.say("x closes a workspace; right-click deletes a worktree"),
                (Some('x'), Row::Ws(_)) => self.request_close(Close::Workspace(ws.path.clone())),
                (Some('q'), _) => self.quit = true,
                _ => {}
            }
        }
    }

    // ---- reorder by drag ----

    /// Where a workspace dragged to screen y lands: (insertion index before removal, row
    /// boundary the indicator line sits on).
    pub(super) fn drop_target(&self, y: f32) -> Option<(usize, usize)> {
        let rows = self.rows();
        let n = rows.len();
        if n == 0 {
            return None;
        }
        let pos = self.sidebar_tree(n).pos_at(y);
        let idx = (pos.floor().max(0.0) as usize).min(n - 1);
        // The block under the mouse: a workspace row plus its worktrees.
        let w = rows[idx].ws();
        let start = rows.iter().position(|r| *r == Row::Ws(w))?;
        let end = subtree_end(&rows, start);
        Some(if pos < (start + end) as f32 / 2.0 { (w, start) } else { (w + 1, end) })
    }

    pub(super) fn drop_row(&mut self, row: Row, to: usize) {
        let Row::Ws(from) = row else { return };
        if !self.row_valid(row) || to > self.workspaces.len() {
            return;
        }
        let ws = self.workspaces.remove(from);
        let to = if to > from { to - 1 } else { to };
        self.workspaces.insert(to, ws);
        self.select_row(Row::Ws(to));
        self.save();
        self.dirty = true;
    }

    // ---- adding ----

    pub(super) fn prompt_add_workspace(&mut self) {
        self.prompt = Some(Prompt::new(PromptKind::AddWorkspace, "Add workspace folder", "~/"));
    }

    pub(super) fn prompt_new_worktree(&mut self, ws: PathBuf) {
        self.prompt = Some(Prompt::new(PromptKind::NewWorktree(ws), "New worktree branch", ""));
    }

    /// The header's +: a workspace at `~`, opened right away (it's just opened if it's there).
    pub(super) fn add_home_workspace(&mut self) {
        let home = std::fs::canonicalize(config::home()).unwrap_or_else(|_| config::home());
        if self.ws_index(&home).is_none() {
            if let Err(e) = self.add_workspace(&home.display().to_string()) {
                return self.say(e);
            }
        }
        self.open_view(home);
    }

    /// Add folder `dir` as a workspace; its name on success.
    pub(super) fn add_workspace(&mut self, dir: &str) -> Result<String, String> {
        let Some(ws) = Workspace::open(&config::expand(dir.trim())) else { return Err(format!("not a directory: {dir}")) };
        if let Some(w) = self.ws_index(&ws.path) {
            self.select_row(Row::Ws(w));
            return Err(format!("{} is already a workspace", ws.name));
        }
        let name = ws.name.clone();
        self.workspaces.push(ws);
        self.select_row(Row::Ws(self.workspaces.len() - 1));
        self.save();
        self.rewatch();
        self.dirty = true;
        Ok(name)
    }

    /// Create a worktree for a new or existing branch, off the UI thread (a checkout of a big
    /// repo takes a while); `Ev::WorktreeAdded` opens it.
    pub(super) fn add_worktree(&mut self, ws: &Path, branch: &str) {
        self.say(format!("creating worktree {branch}..."));
        let (tx, ws, branch) = (self.tx.clone(), ws.to_path_buf(), branch.to_string());
        std::thread::spawn(move || {
            let _ = tx.send_event(Ev::WorktreeAdded(git::add(&ws, &branch)));
        });
    }

    pub(super) fn worktree_added(&mut self, r: Result<PathBuf, String>) {
        match r {
            Ok(dest) => {
                self.reload_git();
                self.open_view(dest);
            }
            Err(e) => self.say(e),
        }
    }

    // ---- closing (a workspace: sidebar only, files are never touched) ----

    /// (display name, views it covers, panes open in them); `None` when it's gone from the sidebar.
    pub(super) fn close_info(&self, c: &Close) -> Option<(String, Vec<PathBuf>, usize)> {
        let (name, paths) = match c {
            Close::Workspace(path) => {
                let ws = &self.workspaces[self.ws_index(path)?];
                (format!("workspace {}", ws.name), ws.view_paths())
            }
            Close::Worktree(_, wt) => (format!("worktree {}", self.names(wt).1), vec![wt.clone()]),
        };
        let n = paths.iter().map(|v| self.panes_in(v).len()).sum();
        Some((name, paths, n))
    }

    /// Close right away if nothing would be lost, otherwise ask first:
    /// - a workspace asks when panes run in it;
    /// - a worktree never asks: it's removed with `--force`, uncommitted and untracked files and all.
    pub(super) fn request_close(&mut self, c: Close) {
        let Some((name, _, n)) = self.close_info(&c) else { return self.say("that's no longer in the sidebar") };
        match c {
            Close::Worktree(_, wt) if self.removing.contains(&wt) => self.say(format!("already deleting {name}")),
            Close::Worktree(proj, wt) => self.start_removal(Removal { proj, wt, name, force: true, reply: None, auto: None }),
            c if n == 0 => self.close(c),
            c => {
                let label = format!("Close {name} and its {}? Files stay on disk. y/n", util::plural(n, "pane"));
                self.prompt = Some(Prompt::new(PromptKind::Confirm(c), label, ""));
            }
        }
    }

    pub(super) fn close(&mut self, c: Close) {
        let Some((name, paths, _)) = self.close_info(&c) else { return self.say("that's no longer in the sidebar") };
        self.leave_views(&paths);
        match c {
            // Off the UI thread: removing a big checkout takes a while. The row stays until done.
            Close::Worktree(proj, wt) => self.start_removal(Removal { proj, wt, name, force: true, reply: None, auto: None }),
            Close::Workspace(path) => {
                if let Some(w) = self.ws_index(&path) {
                    let ws = self.workspaces.remove(w);
                    self.folded.remove(&ws_key(&ws.path));
                    self.save();
                    self.rewatch();
                    self.say(format!("closed {name}"));
                }
            }
        }
        self.sel = self.sel.min(self.rows().len().saturating_sub(1));
        self.dirty = true;
    }

    /// Kill the panes in `paths`, and step out if one of them is the current view.
    pub(super) fn leave_views(&mut self, paths: &[PathBuf]) {
        let ids: Vec<u32> = paths.iter().flat_map(|v| self.panes_in(v)).collect();
        self.kill_panes(ids);
        self.step_out(paths);
    }

    fn step_out(&mut self, paths: &[PathBuf]) {
        if self.current.as_ref().is_some_and(|cur| paths.contains(cur)) {
            self.current = None;
            self.focus = Focus::Sidebar;
            for v in paths {
                self.files.remove(v);
            }
            self.files_rewatch();
        }
    }

    /// Run `git worktree remove` off the UI thread; `Ev::WorktreeRemoved` reports back.
    pub(super) fn start_removal(&mut self, r: Removal) {
        self.say(format!("deleting {}...", r.name));
        self.removing.insert(r.wt.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut r = r;
            if r.auto.is_some() {
                git::drop_untracked_links(&r.wt);
            }
            let res = git::remove(&r.proj, &r.wt, r.force);
            if let (Ok(()), Some(a)) = (&res, r.auto.as_mut()) {
                a.branch_err = git::delete_branch(&r.proj, &a.branch, a.how.force_delete()).err();
            }
            let _ = tx.send_event(Ev::WorktreeRemoved(r, res));
        });
    }

    pub(super) fn worktree_removed(&mut self, r: Removal, res: Result<(), String>) {
        self.removing.remove(&r.wt);
        if let Some(auto) = &r.auto {
            if res.is_ok() {
                self.leave_views(std::slice::from_ref(&r.wt));
            }
            self.merged_removed(&r.wt, &r.name, auto, &res);
            self.reload_git();
            self.sel = self.sel.min(self.rows().len().saturating_sub(1));
            self.dirty = true;
            return;
        }
        let answer = match res {
            Ok(()) => {
                // Its terminals ran in a folder that's gone now.
                self.leave_views(std::slice::from_ref(&r.wt));
                self.say(format!("deleted {}", r.name));
                Reply::ok(format!("deleted {}", config::tilde(&r.wt)))
            }
            Err(e) => {
                let m = format!("couldn't delete {}: {e}", r.name);
                self.say(m.clone());
                Reply::err(m)
            }
        };
        if let Some(tx) = r.reply {
            let _ = tx.send(answer);
        }
        self.reload_git();
        self.sel = self.sel.min(self.rows().len().saturating_sub(1));
        self.dirty = true;
    }
}
