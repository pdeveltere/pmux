//! App model and behaviour: workspaces (folders) → worktrees, panes, keys, mouse, socket API.
//!
//! `App` is one struct; its behaviour is split by concern across the submodules, each adding an
//! `impl App` block: `layout` (geometry), `pane` (terminals), `sidebar`, `agents` (agents panel), `explorer` (file panel),
//! `edit` (editor panes), `find` (go to file), `psearch` (find in project), `navigate` (tabs/focus), `merged` (merged worktrees delete themselves), `keys`, `mouse`, `menu`, `commands` (socket API).
mod adopt;
mod agents;
mod commands;
mod edit;
mod explorer;
mod find;
mod follow;
mod keys;
mod layout;
mod menu;
mod merged;
mod model;
mod mouse;
mod navigate;
mod pane;
mod psearch;
mod restore;
mod sidebar;

pub use agents::{ago, AgentRow, Group, AGENTS_SHARE};
pub use layout::*;
pub use model::*;
pub use pane::{Pane, State};

use crate::api::Reply;
use crate::files::Explorer;
use crate::{config, git, util, watch};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use winit::event::MouseButton;
use winit::event_loop::EventLoopProxy;
use winit::keyboard::ModifiersState;
use winit::window::CursorIcon;

/// How long a status message stays in the status bar.
const FLASH_FOR: Duration = Duration::from_secs(4);
/// Hover time before a tooltip shows, like Zed's.
const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

pub enum Ev {
    /// From the background pmux that holds the terminals.
    Hold(crate::hold::Msg),
    Api(crate::api::Request, Sender<Reply>),
    /// Git metadata changed under these paths (from `git::watch_paths`).
    Git(Vec<PathBuf>),
    /// Behind counts for these worktrees (only nonzero ones in the map).
    Behind(Vec<PathBuf>, HashMap<PathBuf, u32>),
    /// An explorer folder changed on disk.
    Fs,
    /// Files open in editors changed on disk.
    Disk(Vec<PathBuf>),
    /// `git status` for an explorer root finished.
    FileStatus(PathBuf, git::Status),
    /// Whether linked worktree `wt` is merged, as of git state `key` (`git::Refs::merge_key`).
    Merged { proj: PathBuf, wt: PathBuf, branch: String, key: String, how: Option<git::Merge> },
    /// `git worktree remove` finished.
    WorktreeRemoved(Removal, Result<(), String>),
    /// `pmux import`: the plan, each folder's repo, and the caller waiting for the result.
    ImportReady(crate::import::Plan, HashMap<PathBuf, PathBuf>, Sender<Reply>),
    /// The ⌘P popup's file list for this root was read.
    FinderFiles(PathBuf, Vec<String>),
    /// `git worktree add` finished: the new checkout.
    WorktreeAdded(Result<PathBuf, String>),
    /// A background highlight of editor pane `.0` at text version `.1` finished.
    Highlighted(u32, u64, Vec<Vec<crate::syntax::Span>>),
    /// A project search of pane `.0` (generation `.1`) for this query finished.
    Searched(u32, u64, crate::search::Query, crate::search::Found),
    /// pmux → Settings… in the menu bar.
    OpenSettings,
}

pub type Tx = EventLoopProxy<Ev>;

/// A panel that can be hidden and shown again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Panel {
    Sidebar,
    Agents,
    Files,
}

/// Which part of the window takes keyboard input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Panes,
    Sidebar,
    Files,
}

/// A closed terminal pane, enough to open it again (see `App::reopen_closed`).
struct Closed {
    view: PathBuf,
    cwd: PathBuf,
    /// Resumes the agent that ran in it.
    cmd: Option<String>,
}

pub struct App {
    pub workspaces: Vec<Workspace>,
    pub views: HashMap<PathBuf, View>,
    pub panes: HashMap<u32, Pane>,
    next_id: u32,
    pub current: Option<PathBuf>,
    pub focus: Focus,
    pub show_sidebar: bool,
    /// Agents panel shown (at the bottom of the sidebar, when that's shown).
    pub show_agents: bool,
    /// Status bar panel button under the mouse.
    pub status_hover: Option<Panel>,
    /// The sidebar header's + (add workspace) is under the mouse.
    pub plus_hover: bool,
    pub sidebar_w: f32, // logical px
    pub sel: usize,
    pub hover: Option<usize>,
    /// Tab bar part under the mouse.
    pub tab_hover: Option<TabHit>,
    /// Agents panel row under the mouse.
    pub agents_hover: Option<usize>,
    /// Agents panel scroll: first visible row.
    agents_top: usize,
    /// Sidebar scroll: first visible row, and the selection it was last scrolled to show (the
    /// wheel scrolls freely; a selection change scrolls just enough to show it).
    side_top: std::cell::Cell<usize>,
    side_sel: std::cell::Cell<usize>,
    /// Share of the sidebar's height the agents panel takes (drag its top edge to resize).
    pub agents_share: f32,
    /// A done agent you're looking at: listed under Done until focus leaves it.
    held: Option<u32>,
    /// Recently closed tabs, newest last: what ⌘⇧T opens again.
    closed: Vec<Vec<Closed>>,
    pub prefix: bool,
    pub prompt: Option<Prompt>,
    pub folded: HashSet<String>,
    pub flash: Option<(String, Instant)>,
    // Window state, physical px.
    pub w: f32,
    pub h: f32,
    pub scale: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub mods: ModifiersState,
    pub mouse: (f32, f32),
    pub cursor_icon: CursorIcon,
    /// Panel edge being dragged to resize.
    resizing: Option<Edge>,
    /// Sidebar row press / reorder drag in progress.
    pub drag: Option<Drag>,
    mouse_pane: Option<(u32, MouseButton)>,
    /// Editor pane being drag-selected.
    edit_drag: Option<u32>,
    /// Terminal pane being drag-selected.
    term_drag: Option<u32>,
    /// Last terminal click (pane, cell, time, count), for double/triple click.
    term_click: Option<(u32, alacritty_terminal::index::Point, Instant, u8)>,
    /// Last editor click, for double-click.
    edit_click: Option<(crate::editor::Pos, Instant)>,
    wheel_acc: f32,
    /// Font zoom request: +1 / -1 / 0 = reset.
    pub zoom: Option<i32>,
    pub dirty: bool,
    pub quit: bool,
    /// Pending `pmux snapshot <path>`: answered by the renderer after the next frame.
    pub snapshot: Option<(String, Sender<Reply>)>,
    /// Open right-click context menu.
    pub menu: Option<Menu>,
    /// Open "go to file" popup (⌘P).
    pub finder: Option<crate::finder::Finder>,
    /// Files opened in editors, most recent first.
    recent_files: Vec<PathBuf>,
    pub settings: crate::settings::Settings,
    socket: PathBuf,
    tx: Tx,
    /// Link to the background pmux holding the terminals (`None` until attached, or after it stopped).
    hold: Option<crate::hold::Link>,
    /// Fingerprint of the layout last sent to it (`None`: send it next time).
    saved_last: Option<u64>,
    /// Title and state last sent to it, per terminal.
    meta_last: HashMap<u32, crate::hold::Meta>,
    watcher: watch::Watcher,
    /// Commits each worktree is behind its upstream (only nonzero entries).
    pub behind: HashMap<PathBuf, u32>,
    /// File explorer per view root, shown for the current view on the right.
    pub files: HashMap<PathBuf, Explorer>,
    pub show_files: bool,
    pub files_w: f32, // logical px
    pub files_hover: Option<usize>,
    /// Explorer row whose symlink arrow is hovered, and since when (its tooltip shows after `TOOLTIP_DELAY`).
    pub link_hover: Option<(usize, Instant)>,
    files_watcher: watch::Watcher,
    /// Files open in editors, so an edit by another program (an agent) shows.
    disk_watcher: watch::Watcher,
    /// One `git status` at a time; `status_again` re-runs it when changes arrived meanwhile.
    status_busy: bool,
    status_again: bool,
    /// Worktrees being deleted in the background (their sidebar row says so).
    pub removing: HashSet<PathBuf>,
    /// Linked worktrees whose branch is merged, waiting to be deleted (`merged`).
    pub merged: HashMap<PathBuf, merged::Merged>,
    /// Git state each worktree was last checked for being merged at (`git::Refs::merge_key`).
    merge_keys: HashMap<PathBuf, String>,
    /// Why the config file couldn't be loaded; while set, `save` leaves the file alone.
    config_error: Option<String>,
}

impl App {
    pub fn new(tx: Tx, socket: PathBuf) -> Self {
        let default = || {
            let cwd = std::env::current_dir().unwrap_or_default();
            config::Config::new(vec![config::tilde(&cwd)])
        };
        // A config we can't parse is left alone (not overwritten) until the user fixes it.
        let (cfg, config_error) = match config::load() {
            Ok(Some(c)) => (c, None),
            Ok(None) => {
                let c = default();
                config::save(&c);
                (c, None)
            }
            Err(e) => {
                eprintln!("pmux: config: {e}");
                (default(), Some(e))
            }
        };
        let workspaces = cfg.workspaces.iter().filter_map(|p| Workspace::open(&config::expand(p))).collect();
        let flash = config_error.as_ref().map(|e| (format!("config not loaded (changes won't be saved): {e}"), Instant::now()));
        let mut app = App {
            config_error,
            workspaces,
            views: HashMap::new(),
            panes: HashMap::new(),
            next_id: 1,
            current: None,
            focus: Focus::Sidebar,
            show_sidebar: true,
            show_agents: true,
            status_hover: None,
            plus_hover: false,
            sidebar_w: SIDEBAR_W,
            sel: 0,
            hover: None,
            tab_hover: None,
            agents_hover: None,
            agents_top: 0,
            side_top: Default::default(),
            side_sel: Default::default(),
            agents_share: AGENTS_SHARE,
            held: None,
            closed: Vec::new(),
            prefix: false,
            prompt: None,
            folded: HashSet::new(),
            flash,
            w: 1200.0,
            h: 800.0,
            scale: 1.0,
            cell_w: 8.0,
            cell_h: 16.0,
            mods: ModifiersState::empty(),
            mouse: (0.0, 0.0),
            cursor_icon: CursorIcon::Default,
            resizing: None,
            drag: None,
            mouse_pane: None,
            edit_drag: None,
            term_drag: None,
            term_click: None,
            edit_click: None,
            wheel_acc: 0.0,
            zoom: None,
            dirty: true,
            quit: false,
            snapshot: None,
            menu: None,
            finder: None,
            recent_files: Vec::new(),
            settings: Default::default(),
            socket,
            hold: None,
            saved_last: None,
            meta_last: HashMap::new(),
            watcher: watch::Watcher::new(tx.clone(), Ev::Git),
            files_watcher: watch::Watcher::new(tx.clone(), |_| Ev::Fs),
            disk_watcher: watch::Watcher::new(tx.clone(), Ev::Disk),
            tx,
            behind: HashMap::new(),
            files: HashMap::new(),
            show_files: true,
            files_w: FILES_W,
            files_hover: None,
            link_hover: None,
            status_busy: false,
            status_again: false,
            removing: HashSet::new(),
            merged: HashMap::new(),
            merge_keys: HashMap::new(),
        };
        app.rewatch();
        app.reload_settings();
        app
    }

    fn on_hold(&mut self, m: crate::hold::Msg) {
        match m {
            crate::hold::Msg::Output(id, data) => self.dirty |= self.on_output(id, &data),
            crate::hold::Msg::Exit(id) => self.on_exit(id),
            crate::hold::Msg::Agent(id, a) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.set_agent(a.as_deref());
                }
            }
            crate::hold::Msg::Screen(id, st) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.set_screen(st);
                }
            }
            crate::hold::Msg::Cwd(id, dir) => self.follow_cwd(id, &dir),
            crate::hold::Msg::Failed(e) => self.say(format!("spawn failed: {e}")),
            crate::hold::Msg::Lost => self.on_hold_lost(),
        }
    }

    pub fn handle(&mut self, ev: Ev) {
        // Output decides for itself: a pane that isn't on screen only redraws when its sidebar
        // dot changes (agents in background worktrees animate constantly).
        self.dirty |= !matches!(ev, Ev::Hold(crate::hold::Msg::Output(..)));
        match ev {
            Ev::Hold(m) => {
                self.on_hold(m);
                // A merged worktree waits for its busy agents: their state comes from the holder.
                if self.merged.values().any(|m| !m.kept) {
                    self.remove_merged();
                }
            }
            Ev::Api(req, rtx) if req.args.first().map(String::as_str) == Some("snapshot") => {
                let path = req.args.get(1).cloned().unwrap_or_else(|| "pmux-snapshot.bmp".into());
                self.snapshot = Some((path, rtx));
            }
            // Answered when git finishes, so it doesn't hold up the UI thread.
            Ev::Api(req, rtx) if req.args.len() > 2 && req.args[0] == "worktree" && req.args[1] == "rm" => self.api_worktree_rm(&req.args, rtx),
            // Answered once git has looked up the folders' repos, off the UI thread.
            Ev::Api(mut req, rtx) if req.args.len() == 2 && req.args[0] == "import" => self.start_import(req.args.remove(1), rtx),
            Ev::Api(req, rtx) => {
                let r = self.on_api(req);
                let _ = rtx.send(r);
            }
            Ev::Behind(scope, m) => {
                self.behind.retain(|k, _| !scope.contains(k));
                self.behind.extend(m);
            }
            Ev::Git(changed) => self.reload_git_in(Some(&changed)),
            Ev::Fs => {
                let vis = self.files_visible_rows();
                if let Some(e) = self.explorer_mut() {
                    e.rebuild();
                    e.clamp_top(vis);
                }
                self.refresh_status();
            }
            Ev::Disk(changed) => self.on_disk_change(&changed),
            Ev::FileStatus(root, st) => {
                self.status_busy = false;
                if let Some(e) = self.files.get_mut(&root) {
                    e.status = st;
                }
                if std::mem::take(&mut self.status_again) {
                    self.refresh_status();
                }
            }
            Ev::Merged { proj, wt, branch, key, how } => self.merged_checked(proj, wt, branch, key, how),
            Ev::WorktreeRemoved(r, res) => self.worktree_removed(r, res),
            Ev::WorktreeAdded(r) => self.worktree_added(r),
            Ev::FinderFiles(root, files) => self.finder_files(root, files),
            Ev::ImportReady(plan, repos, rtx) => {
                let r = self.api_import(plan, &repos);
                let _ = rtx.send(r);
            }
            Ev::Highlighted(id, version, hl) => {
                if let Some(ed) = self.panes.get_mut(&id).and_then(|p| p.editor.as_mut()) {
                    ed.set_highlight(version, hl);
                }
                self.highlight(id); // edits that came in meanwhile
            }
            Ev::Searched(id, gen, q, f) => self.search_done(id, gen, q, f),
            Ev::OpenSettings => self.open_file(&crate::settings::ensure()),
        }
    }

    // ---- persistence and watching ----

    /// Persist the workspaces (not while the config on disk failed to load).
    fn save(&self) {
        if self.config_error.is_some() {
            return;
        }
        config::save(&config::Config::new(self.workspaces.iter().map(|w| config::tilde(&w.path)).collect()));
    }

    /// Watch every workspace's git metadata, recount every worktree's behind, look for merged ones.
    fn rewatch(&self) {
        self.set_git_watch();
        let wts: Vec<PathBuf> = self.workspaces.iter().flat_map(|p| p.worktrees.iter().flatten()).map(|w| w.path.clone()).collect();
        self.refresh_refs(wts);
    }

    fn set_git_watch(&self) {
        self.watcher.set(self.workspaces.iter().flat_map(|p| git::watch_paths(&p.path)).collect());
    }

    /// Re-read every workspace's worktrees and watch them (we changed them).
    fn reload_git(&mut self) {
        self.reload_git_in(None);
    }

    /// Re-read the worktrees of the workspaces whose git dir holds a `changed` path (all with
    /// `None`), re-watch, and refresh their behind counts and the explorer's git colors.
    fn reload_git_in(&mut self, changed: Option<&[PathBuf]>) {
        let mut views: Vec<PathBuf> = Vec::new();
        for p in self.workspaces.iter_mut() {
            let hit = changed.is_none_or(|c| git::common_dir(&p.path).is_some_and(|d| c.iter().any(|x| x.starts_with(&d))));
            if hit {
                p.worktrees = git::worktrees(&p.path);
                views.push(p.path.clone());
                views.extend(p.worktrees.iter().flatten().map(|w| w.path.clone()));
            }
        }
        if views.is_empty() {
            return;
        }
        self.set_git_watch();
        let all: HashSet<PathBuf> = self.workspaces.iter().flat_map(|p| p.worktrees.iter().flatten()).map(|w| w.path.clone()).collect();
        self.merged.retain(|k, _| all.contains(k));
        self.merge_keys.retain(|k, _| all.contains(k));
        let wts: Vec<PathBuf> = all.into_iter().filter(|w| views.contains(w)).collect();
        self.refresh_refs(wts);
        if self.explorer().is_some_and(|e| views.contains(&e.root)) {
            self.refresh_status();
        }
    }

    // ---- views ----

    /// (workspace, worktree) names for a view path.
    pub fn names(&self, view: &Path) -> (String, String) {
        for ws in &self.workspaces {
            if ws.path == view {
                return (ws.name.clone(), ws.branch().unwrap_or(&ws.name).to_string());
            }
            if let Some(wt) = ws.worktrees.iter().flatten().find(|w| w.path == view) {
                return (ws.name.clone(), wt.branch.clone());
            }
        }
        let n = util::file_name(view);
        (n.clone(), n)
    }

    /// Every pane in a view, across all its tabs.
    pub fn panes_in(&self, view: &Path) -> Vec<u32> {
        self.views.get(view).map(|v| v.tabs.iter().flat_map(|t| t.panes.iter().copied()).collect()).unwrap_or_default()
    }

    /// Panes of the view's shown tab.
    pub fn tab_panes(&self, view: &Path) -> &[u32] {
        self.views.get(view).and_then(View::cur).map(|t| t.panes.as_slice()).unwrap_or(&[])
    }

    pub fn current_view(&self) -> Option<&View> {
        self.views.get(self.current.as_ref()?)
    }

    fn current_view_mut(&mut self) -> Option<&mut View> {
        self.views.get_mut(self.current.as_ref()?)
    }

    /// The focused pane of the shown tab.
    pub fn focused(&self) -> Option<u32> {
        let t = self.current_view()?.cur()?;
        t.panes.get(t.focus).copied()
    }

    pub fn open_view(&mut self, view: PathBuf) {
        if self.panes_in(&view).is_empty() {
            if let Err(e) = self.spawn_pane(&view, &view, false) {
                return self.say(format!("spawn failed: {e}"));
            }
        }
        if let Some(i) = self.rows().iter().position(|r| self.row_view(*r).as_ref() == Some(&view)) {
            self.sel = i;
        }
        self.files.entry(view.clone()).or_insert_with(|| Explorer::new(view.clone()));
        self.current = Some(view);
        self.focus_panes();
        self.files_rewatch();
        self.refresh_status();
    }

    /// Open the cwd's view on launch, if it's in the sidebar.
    pub fn open_cwd(&mut self) {
        if let Ok(cwd) = std::env::current_dir().and_then(std::fs::canonicalize) {
            if self.rows().iter().any(|r| self.row_view(*r).as_ref() == Some(&cwd)) {
                self.open_view(cwd);
            }
        }
    }

    // ---- focus and feedback ----

    /// Keyboard to the panes; looking at a pane clears its bell.
    fn focus_panes(&mut self) {
        self.focus = Focus::Panes;
        self.see_focused();
    }

    /// Back to the panes if there's one to focus, else the sidebar.
    fn focus_default(&mut self) {
        self.focus = if self.focused().is_some() { Focus::Panes } else { Focus::Sidebar };
    }

    /// ⌘E: sidebar ↔ panes (showing the sidebar if it's hidden).
    fn toggle_sidebar_focus(&mut self) {
        self.focus = if self.focus == Focus::Sidebar && self.focused().is_some() { Focus::Panes } else { Focus::Sidebar };
        if self.focus == Focus::Sidebar {
            self.show_sidebar = true;
        }
    }

    /// Is `p` on screen?
    pub fn panel_shown(&self, p: Panel) -> bool {
        match p {
            Panel::Sidebar => self.show_sidebar,
            Panel::Agents => self.show_sidebar && self.show_agents,
            Panel::Files => self.show_files && self.explorer().is_some(),
        }
    }

    /// Hide `p` if it's shown, else show it (the agents panel brings the sidebar with it).
    pub(super) fn toggle_panel(&mut self, p: Panel) {
        let on = !self.panel_shown(p);
        match p {
            Panel::Sidebar => {
                self.show_sidebar = on;
                if !on {
                    self.hover = None;
                    self.agents_hover = None;
                    self.drag = None;
                    if self.focus == Focus::Sidebar {
                        self.focus_default();
                    }
                }
            }
            Panel::Agents => {
                self.show_agents = on;
                self.agents_hover = None;
                self.show_sidebar |= on;
            }
            Panel::Files if on => self.show_files_panel(),
            Panel::Files => self.hide_files(),
        }
    }

    fn see_focused(&mut self) {
        let f = self.focused();
        // Looking at a done agent: it turns idle, but stays listed under Done until you leave it.
        if f != self.held {
            self.held = f.filter(|id| self.panes.get(id).and_then(Pane::state) == Some(State::Done));
        }
        if let Some(p) = f.and_then(|id| self.panes.get_mut(&id)) {
            p.bell = false;
        }
        let shown: Vec<u32> = self.current.as_ref().map(|v| self.tab_panes(v).to_vec()).unwrap_or_default();
        for id in shown {
            if let Some(p) = self.panes.get_mut(&id) {
                p.unseen = false;
            }
        }
    }

    fn toggle_fold(&mut self, key: String) {
        util::toggle(&mut self.folded, key);
    }

    /// Show a message in the status bar for a few seconds.
    pub fn say(&mut self, m: impl Into<String>) {
        self.flash = Some((m.into(), Instant::now()));
    }

    /// Re-read `settings.toml`, keeping the current settings when it doesn't parse. Whether it did.
    pub(super) fn reload_settings(&mut self) -> bool {
        match crate::settings::load() {
            Ok(s) => {
                self.settings = s;
                self.dirty = true;
                true
            }
            Err(e) => {
                self.say(e);
                false
            }
        }
    }

    /// Explorer row whose symlink tooltip shows, once the arrow has been hovered long enough.
    pub fn link_tooltip(&self) -> Option<usize> {
        self.link_hover.filter(|(_, t)| t.elapsed() >= TOOLTIP_DELAY && self.menu.is_none()).map(|(i, _)| i)
    }

    /// The status bar message, while it lasts.
    pub fn flash_text(&self) -> Option<&str> {
        self.flash.as_ref().filter(|(_, t)| t.elapsed() < FLASH_FOR).map(|(m, _)| m.as_str())
    }

    /// When a time-based state changes next (working → idle heuristic, flash expiry). No idle ticks.
    pub fn next_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let flash = self.flash.as_ref().map(|(_, t)| *t + FLASH_FOR).filter(|f| *f > now);
        let tooltip = self.link_hover.map(|(_, t)| t + TOOLTIP_DELAY).filter(|t| *t > now);
        self.panes.values().filter_map(Pane::state_change_at).chain(flash).chain(tooltip).chain(self.agents_deadline()).min()
    }
}
