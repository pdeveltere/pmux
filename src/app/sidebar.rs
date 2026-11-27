//! Sidebar: the workspace → project → worktree tree, its keys, reordering, adding and closing.
use super::{proj_key, subtree_end, ws_key, App, Close, Drop, Ev, Focus, Project, Prompt, PromptKind, Removal, Row, Workspace};
use crate::api::Reply;
use crate::{config, git, input};
use std::path::{Path, PathBuf};
use winit::event::KeyEvent;
use winit::keyboard::NamedKey;

impl App {
    /// The visible sidebar rows, top to bottom.
    pub fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        // The row being typed in, if any: first under the parent it adds to.
        let inline = self.prompt.as_ref().filter(|p| p.inline).map(|p| &p.kind);
        for (wi, ws) in self.workspaces.iter().enumerate() {
            out.push(Row::Ws(wi));
            if self.folded.contains(&ws_key(&ws.name)) {
                continue;
            }
            if matches!(inline, Some(PromptKind::AddProject(name)) if *name == ws.name) {
                out.push(Row::New(wi, 1));
            }
            for (pi, p) in ws.projects.iter().enumerate() {
                out.push(Row::Proj(wi, pi));
                if matches!(inline, Some(PromptKind::NewWorktree(path)) if *path == p.path) {
                    out.push(Row::New(wi, 2));
                }
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

    /// The view a row opens (a workspace's: its own folder).
    pub fn row_view(&self, r: Row) -> Option<PathBuf> {
        match r {
            Row::Ws(wi) => Some(self.workspaces[wi].path.clone()),
            Row::Proj(wi, pi) => Some(self.project(wi, pi).path.clone()),
            Row::Wt(wi, pi, ti) => Some(self.project(wi, pi).worktrees.as_ref()?[ti].path.clone()),
            Row::New(..) => None,
        }
    }

    /// The fold key of a row that can fold.
    fn fold_key(&self, row: Row) -> Option<String> {
        match row {
            Row::Ws(w) if !self.workspaces[w].projects.is_empty() => Some(ws_key(&self.workspaces[w].name)),
            Row::Proj(w, p) if self.project(w, p).has_linked() => Some(proj_key(&self.project(w, p).path)),
            _ => None,
        }
    }

    fn row_valid(&self, row: Row) -> bool {
        match row {
            Row::Ws(w) => w < self.workspaces.len(),
            Row::Proj(w, p) => self.workspaces.get(w).is_some_and(|ws| p < ws.projects.len()),
            Row::Wt(w, p, t) => self.workspaces.get(w).and_then(|ws| ws.projects.get(p)).and_then(|pr| pr.worktrees.as_ref()).is_some_and(|v| t < v.len()),
            Row::New(..) => false,
        }
    }

    fn select_row(&mut self, row: Row) {
        if let Some(i) = self.rows().iter().position(|r| *r == row) {
            self.sel = i;
        }
    }

    /// Workspace by name.
    pub(super) fn ws_index(&self, name: &str) -> Option<usize> {
        self.workspaces.iter().position(|w| w.name == name)
    }

    /// (workspace, project) of the project at `path`.
    pub(super) fn proj_index(&self, path: &Path) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(w, ws)| ws.projects.iter().position(|p| p.path == path).map(|p| (w, p)))
    }

    /// The close target for a workspace or project row.
    fn close_of(&self, row: Row) -> Option<Close> {
        match row {
            Row::Ws(w) => Some(Close::Workspace(self.workspaces[w].name.clone())),
            Row::Proj(w, p) => Some(Close::Project(self.project(w, p).path.clone())),
            Row::Wt(..) | Row::New(..) => None,
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
                Some('W' | 'a') => self.prompt_new_workspace(),
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
                (Some('x'), Row::Wt(..)) => self.say("x closes a project or workspace; right-click deletes a worktree"),
                (Some('x'), _) => {
                    if let Some(c) = self.close_of(row) {
                        self.request_close(c);
                    }
                }
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
                Row::New(..) => None,
            },
            Row::Wt(..) | Row::New(..) => None,
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
        self.prompt = Some(Prompt::new(PromptKind::NewWorkspace, "New workspace folder", "~/"));
    }

    /// The header's +: a workspace at `~` at the top, opened right away (an existing one moves up).
    pub(super) fn add_home_workspace(&mut self) {
        let home = std::fs::canonicalize(config::home()).unwrap_or_else(|_| config::home());
        match self.workspaces.iter().position(|w| w.path == home) {
            Some(i) => {
                let ws = self.workspaces.remove(i);
                self.workspaces.insert(0, ws);
                self.save();
                self.dirty = true;
            }
            None => {
                if let Err(e) = self.add_workspace(&home.display().to_string()) {
                    return self.say(e);
                }
            }
        }
        self.open_view(home);
    }

    /// Type the new worktree's branch in a new row under its project.
    pub(super) fn prompt_new_worktree(&mut self, project: PathBuf) {
        let Some((w, _)) = self.proj_index(&project) else { return self.say("that project is no longer in the sidebar") };
        self.folded.remove(&ws_key(&self.workspaces[w].name));
        self.folded.remove(&proj_key(&project));
        self.start_inline(PromptKind::NewWorktree(project));
    }

    /// Open an inline prompt: its row appears in the tree, selected (so it scrolls into view).
    fn start_inline(&mut self, kind: PromptKind) {
        self.show_sidebar = true;
        self.prompt = Some(Prompt::inline(kind));
        if let Some(i) = self.rows().iter().position(|r| matches!(r, Row::New(..))) {
            self.sel = i;
        }
        self.dirty = true;
    }

    /// An inline prompt `p` is done (taken out of `self.prompt`): select the row it was adding to.
    pub(super) fn end_inline(&mut self, p: &Prompt) {
        if !p.inline {
            return;
        }
        let parent = match &p.kind {
            PromptKind::AddProject(ws) => self.ws_index(ws).map(Row::Ws),
            PromptKind::NewWorktree(proj) => self.proj_index(proj).map(|(w, p)| Row::Proj(w, p)),
            _ => None,
        };
        match parent {
            Some(r) => self.select_row(r),
            None => self.sel = self.sel.min(self.rows().len().saturating_sub(1)),
        }
    }

    /// Add a project to workspace `w` (default: the selected row's workspace, else the last one).
    pub(super) fn prompt_add_project(&mut self, w: Option<usize>) {
        let w = match w.or_else(|| self.selected_ws()).or(self.workspaces.len().checked_sub(1)) {
            Some(w) => w,
            None => {
                // No workspace yet: make one at ~ so the project has a home.
                if let Err(e) = self.add_workspace("~") {
                    return self.say(e);
                }
                0
            }
        };
        let name = self.workspaces[w].name.clone();
        self.folded.remove(&ws_key(&name));
        self.start_inline(PromptKind::AddProject(name));
    }

    /// The inline new project `name`: a folder in workspace `w`'s (or an absolute / `~` path), made
    /// if it doesn't exist yet.
    pub(super) fn new_project(&mut self, w: usize, name: &str) -> Result<String, String> {
        let name = name.trim();
        let dir = if name.starts_with(['/', '~']) { config::expand(name) } else { self.workspaces[w].path.join(name) };
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {}: {e}", config::tilde(&dir)))?;
        }
        self.add_project(w, &dir.display().to_string())
    }

    /// Add folder `dir` as a workspace at the top, named after it ("api 2" when "api" is taken); its name.
    pub(super) fn add_workspace(&mut self, dir: &str) -> Result<String, String> {
        let path = std::fs::canonicalize(config::expand(dir.trim())).ok().filter(|p| p.is_dir()).ok_or(format!("not a directory: {dir}"))?;
        if let Some(ws) = self.workspaces.iter().find(|w| w.path == path) {
            return Err(format!("{} is already a workspace", ws.name));
        }
        let ws = Workspace::new(path, vec![], &self.workspaces);
        let name = ws.name.clone();
        self.workspaces.insert(0, ws);
        self.select_row(Row::Ws(0));
        self.save();
        self.dirty = true;
        Ok(name)
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

    /// Create a worktree for a new or existing branch, off the UI thread (a checkout of a big
    /// repo takes a while); `Ev::WorktreeAdded` opens it.
    pub(super) fn add_worktree(&mut self, project: &Path, branch: &str) {
        self.say(format!("creating worktree {branch}..."));
        let (tx, project, branch) = (self.tx.clone(), project.to_path_buf(), branch.to_string());
        std::thread::spawn(move || {
            let _ = tx.send_event(Ev::WorktreeAdded(git::add(&project, &branch)));
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

    // ---- closing (workspaces / projects: sidebar only, files are never touched) ----

    /// (display name, views it covers, panes open in them); `None` when it's gone from the sidebar.
    pub(super) fn close_info(&self, c: &Close) -> Option<(String, Vec<PathBuf>, usize)> {
        let (name, paths) = match c {
            Close::Workspace(n) => {
                let ws = &self.workspaces[self.ws_index(n)?];
                (format!("workspace {}", ws.name), ws.view_paths())
            }
            Close::Project(path) => {
                let (w, p) = self.proj_index(path)?;
                let pr = self.project(w, p);
                (format!("project {}", pr.name), pr.view_paths())
            }
            Close::Worktree(_, wt) => (format!("worktree {}", self.names(wt).2), vec![wt.clone()]),
        };
        let n = paths.iter().map(|v| self.panes_in(v).len()).sum();
        Some((name, paths, n))
    }

    /// Close right away, never asking: a workspace or project only leaves the sidebar (its panes close, files
    /// stay); a worktree is removed with `--force`, uncommitted and untracked files and all.
    pub(super) fn request_close(&mut self, c: Close) {
        let Some((name, _, _)) = self.close_info(&c) else { return self.say("that's no longer in the sidebar") };
        match c {
            Close::Worktree(_, wt) if self.removing.contains(&wt) => self.say(format!("already deleting {name}")),
            Close::Worktree(proj, wt) => self.start_removal(Removal { proj, wt, name, force: true, reply: None, auto: None }),
            c => self.close(c),
        }
    }

    pub(super) fn close(&mut self, c: Close) {
        let Some((name, paths, _)) = self.close_info(&c) else { return self.say("that's no longer in the sidebar") };
        self.leave_views(&paths);
        match c {
            // Off the UI thread: removing a big checkout takes a while. The row stays until done.
            Close::Worktree(proj, wt) => self.start_removal(Removal { proj, wt, name, force: true, reply: None, auto: None }),
            Close::Workspace(n) => {
                if let Some(w) = self.ws_index(&n) {
                    let ws = self.workspaces.remove(w);
                    self.folded.remove(&ws_key(&ws.name));
                    self.closed_tree(format!("closed {name}"));
                }
            }
            Close::Project(path) => {
                if let Some((w, p)) = self.proj_index(&path) {
                    let pr = self.workspaces[w].projects.remove(p);
                    self.folded.remove(&proj_key(&pr.path));
                    self.closed_tree(format!("closed {name}"));
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

    /// After removing a workspace or project from the tree.
    fn closed_tree(&mut self, msg: String) {
        self.save();
        self.rewatch();
        self.say(msg);
    }
}
