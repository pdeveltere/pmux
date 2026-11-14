//! App model and behaviour: workspaces → projects → worktrees, panes, keys, mouse, socket API.
//!
//! `App` is one struct; its behaviour is split by concern across the submodules, each adding an
//! `impl App` block: `layout` (geometry), `pane` (terminals), `sidebar`, `explorer` (file panel),
//! `edit` (editor panes), `navigate` (tabs/focus), `keys`, `mouse`, `menu`, `commands` (socket API).
mod commands;
mod edit;
mod explorer;
mod keys;
mod layout;
mod menu;
mod model;
mod mouse;
mod navigate;
mod pane;
mod sidebar;

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

pub enum Ev {
    Output(u32, Vec<u8>),
    Exit(u32),
    Api(crate::api::Request, Sender<Reply>),
    Git,
    Behind(HashMap<PathBuf, u32>),
    /// An explorer folder changed on disk.
    Fs,
    /// `git status` for an explorer root finished.
    FileStatus(PathBuf, git::Status),
}

pub type Tx = EventLoopProxy<Ev>;

/// Which part of the window takes keyboard input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Panes,
    Sidebar,
    Files,
}

pub struct App {
    pub workspaces: Vec<Workspace>,
    pub views: HashMap<PathBuf, View>,
    pub panes: HashMap<u32, Pane>,
    next_id: u32,
    pub current: Option<PathBuf>,
    pub focus: Focus,
    pub show_sidebar: bool,
    pub sidebar_w: f32, // logical px
    pub sel: usize,
    pub hover: Option<usize>,
    /// Tab bar part under the mouse.
    pub tab_hover: Option<TabHit>,
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
    socket: PathBuf,
    tx: Tx,
    watcher: watch::Watcher,
    /// Commits each worktree is behind its upstream (only nonzero entries).
    pub behind: HashMap<PathBuf, u32>,
    /// File explorer per view root, shown for the current view on the right.
    pub files: HashMap<PathBuf, Explorer>,
    pub show_files: bool,
    pub files_w: f32, // logical px
    pub files_hover: Option<usize>,
    files_watcher: watch::Watcher,
    /// One `git status` at a time; `status_again` re-runs it when changes arrived meanwhile.
    status_busy: bool,
    status_again: bool,
}

impl App {
    pub fn new(tx: Tx, socket: PathBuf) -> Self {
        let cfg = config::load().unwrap_or_else(|| {
            let cwd = std::env::current_dir().unwrap_or_default();
            let c = config::Config { workspace: vec![config::WsCfg { name: "default".into(), projects: vec![cwd.display().to_string()] }] };
            config::save(&c);
            c
        });
        let workspaces = cfg
            .workspace
            .into_iter()
            .map(|w| Workspace { name: w.name, projects: w.projects.iter().filter_map(|p| Project::open(&config::expand(p))).collect() })
            .collect();
        let app = App {
            workspaces,
            views: HashMap::new(),
            panes: HashMap::new(),
            next_id: 1,
            current: None,
            focus: Focus::Sidebar,
            show_sidebar: true,
            sidebar_w: SIDEBAR_W,
            sel: 0,
            hover: None,
            tab_hover: None,
            prefix: false,
            prompt: None,
            folded: HashSet::new(),
            flash: None,
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
            edit_click: None,
            wheel_acc: 0.0,
            zoom: None,
            dirty: true,
            quit: false,
            snapshot: None,
            menu: None,
            socket,
            watcher: watch::Watcher::new(tx.clone(), || Ev::Git),
            files_watcher: watch::Watcher::new(tx.clone(), || Ev::Fs),
            tx,
            behind: HashMap::new(),
            files: HashMap::new(),
            show_files: true,
            files_w: FILES_W,
            files_hover: None,
            status_busy: false,
            status_again: false,
        };
        app.rewatch();
        app
    }

    pub fn handle(&mut self, ev: Ev) {
        self.dirty = true;
        match ev {
            Ev::Output(id, data) => self.on_output(id, &data),
            Ev::Exit(id) => self.on_exit(id),
            Ev::Api(req, rtx) if req.args.first().map(String::as_str) == Some("snapshot") => {
                let path = req.args.get(1).cloned().unwrap_or_else(|| "pmux-snapshot.bmp".into());
                self.snapshot = Some((path, rtx));
            }
            Ev::Api(req, rtx) => {
                let r = self.on_api(req);
                let _ = rtx.send(r);
            }
            Ev::Behind(m) => self.behind = m,
            Ev::Git => self.reload_git(),
            Ev::Fs => {
                let vis = self.files_visible_rows();
                if let Some(e) = self.explorer_mut() {
                    e.rebuild();
                    e.clamp_top(vis);
                }
                self.refresh_status();
            }
            Ev::FileStatus(root, st) => {
                self.status_busy = false;
                if let Some(e) = self.files.get_mut(&root) {
                    e.status = st;
                }
                if std::mem::take(&mut self.status_again) {
                    self.refresh_status();
                }
            }
        }
    }

    // ---- persistence and watching ----

    fn save(&self) {
        let c = config::Config {
            workspace: self
                .workspaces
                .iter()
                .map(|w| config::WsCfg { name: w.name.clone(), projects: w.projects.iter().map(|p| config::tilde(&p.path)).collect() })
                .collect(),
        };
        config::save(&c);
    }

    fn projects(&self) -> impl Iterator<Item = &Project> {
        self.workspaces.iter().flat_map(|w| &w.projects)
    }

    fn rewatch(&self) {
        self.watcher.set(self.projects().flat_map(|p| git::watch_paths(&p.path)).collect());
        self.refresh_behind();
    }

    /// Off the UI thread: one `git rev-list` per worktree.
    fn refresh_behind(&self) {
        let wts: Vec<PathBuf> = self.projects().flat_map(|p| p.worktrees.iter().flatten()).map(|w| w.path.clone()).collect();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let m = wts.into_iter().map(|p| (git::behind(&p), p)).filter(|(n, _)| *n > 0).map(|(n, p)| (p, n)).collect();
            let _ = tx.send_event(Ev::Behind(m));
        });
    }

    /// Re-read every project's worktrees and watch them (git metadata changed, or we changed it).
    fn reload_git(&mut self) {
        for p in self.workspaces.iter_mut().flat_map(|w| &mut w.projects) {
            p.worktrees = git::worktrees(&p.path);
        }
        self.rewatch();
        self.refresh_status();
    }

    // ---- views ----

    /// (workspace, project, worktree) names for a view path.
    pub fn names(&self, view: &Path) -> (String, String, String) {
        for ws in &self.workspaces {
            for p in &ws.projects {
                if p.path == view {
                    let b = p.branch().unwrap_or(&p.name).to_string();
                    return (ws.name.clone(), p.name.clone(), b);
                }
                if let Some(wt) = p.worktrees.iter().flatten().find(|w| w.path == view) {
                    return (ws.name.clone(), p.name.clone(), wt.branch.clone());
                }
            }
        }
        let n = util::file_name(view);
        (String::new(), n.clone(), n)
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

    fn open_view(&mut self, view: PathBuf) {
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

    /// ⌘E: sidebar ↔ panes.
    fn toggle_sidebar_focus(&mut self) {
        self.focus = if self.focus == Focus::Sidebar && self.focused().is_some() { Focus::Panes } else { Focus::Sidebar };
    }

    fn see_focused(&mut self) {
        if let Some(p) = self.focused().and_then(|id| self.panes.get_mut(&id)) {
            p.bell = false;
        }
    }

    fn toggle_fold(&mut self, key: String) {
        util::toggle(&mut self.folded, key);
    }

    /// Show a message in the status bar for a few seconds.
    fn say(&mut self, m: impl Into<String>) {
        self.flash = Some((m.into(), Instant::now()));
    }

    /// The status bar message, while it lasts.
    pub fn flash_text(&self) -> Option<&str> {
        self.flash.as_ref().filter(|(_, t)| t.elapsed() < FLASH_FOR).map(|(m, _)| m.as_str())
    }

    /// When a time-based state changes next (working → idle heuristic, flash expiry). No idle ticks.
    pub fn next_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let flash = self.flash.as_ref().map(|(_, t)| *t + FLASH_FOR).filter(|f| *f > now);
        self.panes.values().filter_map(Pane::state_change_at).chain(flash).min()
    }
}
