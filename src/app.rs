//! App model and behaviour: workspaces → projects → worktrees, panes, keys, mouse, socket API.
use crate::api::{Reply, Request};
use crate::term::Vt;
use crate::{config, git, input, pty, watch};
use alacritty_terminal::event::{Event as TEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use winit::event::{ElementState, KeyEvent, MouseButton};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

const WORKING_QUIET: Duration = Duration::from_millis(1500);

// Layout, in logical px (multiplied by the scale factor).
pub const SIDEBAR_W: f32 = 250.0;
pub const SB_TOP: f32 = 10.0;
pub const ROW_H: f32 = 24.0;
/// Tree indent per level (Zed's project panel uses 20).
pub const INDENT: f32 = 16.0;
/// Row inset from the sidebar edges.
pub const ROW_INSET: f32 = 6.0;
pub const STATUS_H: f32 = 26.0;
pub const PAD: f32 = 8.0;

pub enum Ev {
    Output(u32, Vec<u8>),
    Exit(u32),
    Api(Request, Sender<Reply>),
    Git,
    Behind(HashMap<PathBuf, u32>),
}

pub type Tx = EventLoopProxy<Ev>;

#[derive(Clone, Copy, Default, Debug)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

pub struct PaneRect {
    pub id: u32,
    pub header: R,
    pub body: R,
}

pub struct Pane {
    pub id: u32,
    pub vt: Vt,
    master: File,
    shell: i32,
    pgid: i32,
    pub view: PathBuf,
    pub title: Option<String>,
    pub osc_title: Option<String>,
    pub state: Option<String>,
    pub agent: Option<&'static str>,
    pub bell: bool,
    last_output: Instant,
    last_input: Instant,
    last_probe: Instant,
    pub rows: usize,
    pub cols: usize,
}

impl Pane {
    pub fn state(&self) -> Option<&str> {
        if let Some(s) = &self.state {
            return Some(s);
        }
        if self.bell {
            return Some("waiting");
        }
        self.agent?;
        Some(if self.last_output.elapsed() < WORKING_QUIET { "working" } else { "idle" })
    }
    pub fn label(&self) -> String {
        self.title.clone().or_else(|| self.osc_title.clone()).or(self.agent.map(String::from)).unwrap_or_else(|| "shell".into())
    }
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
    pub fn view_paths(&self) -> Vec<PathBuf> {
        let mut v = vec![self.path.clone()];
        v.extend(self.worktrees.iter().flatten().filter(|w| !w.main).map(|w| w.path.clone()));
        v
    }
}

pub struct Workspace {
    pub name: String,
    pub projects: Vec<Project>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Row {
    Ws(usize),
    Proj(usize, usize),
    Wt(usize, usize, usize),
}

#[derive(Default)]
pub struct View {
    pub panes: Vec<u32>,
    pub focus: usize,
}

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
    AddProject(usize),
    NewWorkspace,
    RenameWorkspace(usize),
    Toggle(String),
    NewWorktree(PathBuf),
    Open(PathBuf),
    CopyPath(PathBuf),
    Reveal(PathBuf),
    Delete(Close),
    Sep,
}

pub struct Menu {
    pub x: f32,
    pub y: f32,
    pub items: Vec<(String, Action)>,
    pub hover: Option<usize>,
}

pub enum PromptKind {
    AddProject(usize),
    NewWorkspace,
    NewWorktree(PathBuf),
    /// Yes/no before closing something that still has panes.
    Confirm(Close),
    RenameWorkspace(usize),
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
    start_y: f32,
    pub active: bool,
    /// (target, row boundary the indicator line sits on, tree level of the line).
    pub target: Option<(Drop, usize, usize)>,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub input: String,
}

impl Prompt {
    fn new(kind: PromptKind, label: &str, input: &str) -> Self {
        Prompt { kind, label: label.into(), input: input.into() }
    }
}

pub struct App {
    pub workspaces: Vec<Workspace>,
    pub views: HashMap<PathBuf, View>,
    pub panes: HashMap<u32, Pane>,
    next_id: u32,
    pub current: Option<PathBuf>,
    pub sidebar_focus: bool,
    pub show_sidebar: bool,
    pub sidebar_w: f32, // logical px
    pub sel: usize,
    pub hover: Option<usize>,
    /// Sidebar button under the mouse: 0 none, 2 row +.
    pub hover_btn: u8,
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
    drag_sidebar: bool,
    /// Sidebar row press / reorder drag in progress.
    pub drag: Option<Drag>,
    mouse_pane: Option<(u32, MouseButton)>,
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
}

pub fn ws_key(name: &str) -> String {
    format!("w:{name}")
}
pub fn proj_key(p: &Path) -> String {
    format!("p:{}", p.display())
}

#[derive(Clone, Copy)]
enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    WheelUp,
    WheelDown,
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
            .map(|w| Workspace { name: w.name, projects: w.projects.iter().filter_map(|p| make_project(&config::expand(p))).collect() })
            .collect();
        let watcher = watch::Watcher::new(tx.clone());
        let app = App {
            workspaces,
            views: HashMap::new(),
            panes: HashMap::new(),
            next_id: 1,
            current: None,
            sidebar_focus: true,
            show_sidebar: true,
            sidebar_w: SIDEBAR_W,
            sel: 0,
            hover: None,
            hover_btn: 0,
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
            drag_sidebar: false,
            drag: None,
            mouse_pane: None,
            wheel_acc: 0.0,
            zoom: None,
            dirty: true,
            quit: false,
            menu: None,
            snapshot: None,
            socket,
            tx,
            watcher,
            behind: HashMap::new(),
        };
        app.rewatch();
        app
    }

    pub fn px(&self, logical: f32) -> f32 {
        (logical * self.scale).round()
    }

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

    fn rewatch(&self) {
        let paths = self.workspaces.iter().flat_map(|w| &w.projects).flat_map(|p| git::watch_paths(&p.path)).collect();
        self.watcher.set(paths);
        self.refresh_behind();
    }

    /// Off the UI thread: one `git rev-list` per worktree.
    fn refresh_behind(&self) {
        let wts: Vec<PathBuf> = self.workspaces.iter().flat_map(|w| &w.projects).flat_map(|p| p.worktrees.iter().flatten()).map(|w| w.path.clone()).collect();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let m = wts.into_iter().filter_map(|p| Some((git::behind(&p), p)).filter(|(n, _)| *n > 0).map(|(n, p)| (p, n))).collect();
            let _ = tx.send_event(Ev::Behind(m));
        });
    }

    fn refresh_git(&mut self) {
        for p in self.workspaces.iter_mut().flat_map(|w| &mut w.projects) {
            p.worktrees = git::worktrees(&p.path);
        }
    }

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

    pub fn row_view(&self, r: Row) -> Option<PathBuf> {
        match r {
            Row::Proj(wi, pi) => Some(self.workspaces[wi].projects[pi].path.clone()),
            Row::Wt(wi, pi, ti) => Some(self.workspaces[wi].projects[pi].worktrees.as_ref()?[ti].path.clone()),
            Row::Ws(_) => None,
        }
    }

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
        let n = view.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        (String::new(), n.clone(), n)
    }

    pub fn panes_in(&self, view: &Path) -> &[u32] {
        self.views.get(view).map(|v| v.panes.as_slice()).unwrap_or(&[])
    }

    // ---- geometry (physical px) ----

    pub fn sidebar_rect(&self) -> R {
        if !self.show_sidebar {
            return R::default();
        }
        R { x: 0.0, y: 0.0, w: self.px(self.sidebar_w), h: self.h - self.px(STATUS_H) }
    }

    pub fn status_rect(&self) -> R {
        let h = self.px(STATUS_H);
        R { x: 0.0, y: self.h - h, w: self.w, h }
    }

    pub fn pane_area(&self) -> R {
        let x = self.sidebar_rect().w;
        R { x, y: 0.0, w: (self.w - x).max(0.0), h: (self.h - self.px(STATUS_H)).max(0.0) }
    }

    /// Current view's panes, side by side with 1px separators.
    pub fn layout(&self) -> Vec<PaneRect> {
        let Some(cur) = &self.current else { return vec![] };
        let ids = self.panes_in(cur);
        let a = self.pane_area();
        let n = ids.len();
        if n == 0 {
            return vec![];
        }
        let sep = 1.0f32.max(self.scale.floor());
        let colw = ((a.w - sep * (n - 1) as f32) / n as f32).floor();
        let pad = self.px(PAD);
        let mut x = a.x;
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                let w = if i == n - 1 { a.x + a.w - x } else { colw };
                // No header bar: `header` is the whole column (focus/click target), `body` the cell grid.
                let header = R { x, y: a.y, w, h: a.h };
                let body = R { x: x + pad, y: a.y + pad, w: (w - 2.0 * pad).max(0.0), h: (a.h - 2.0 * pad).max(0.0) };
                x += w + sep;
                PaneRect { id: *id, header, body }
            })
            .collect()
    }

    pub fn grid_size(&self, body: R) -> (usize, usize) {
        (((body.w / self.cell_w).floor() as usize).max(2), ((body.h / self.cell_h).floor() as usize).max(1))
    }

    pub fn apply_layout(&mut self) {
        for pr in self.layout() {
            let (cols, rows) = self.grid_size(pr.body);
            let (cw, ch) = (self.cell_w as u16, self.cell_h as u16);
            let p = self.panes.get_mut(&pr.id).unwrap();
            if p.rows != rows || p.cols != cols {
                p.rows = rows;
                p.cols = cols;
                p.vt.resize(cols, rows);
                pty::resize(&p.master, rows as u16, cols as u16, cols as u16 * cw, rows as u16 * ch);
            }
        }
    }

    /// First visible sidebar row index.
    pub fn sidebar_top(&self, nrows: usize) -> usize {
        let vis = self.sidebar_visible_rows();
        let sel = self.sel.min(nrows.saturating_sub(1));
        (sel + 1).saturating_sub(vis)
    }

    pub fn sidebar_visible_rows(&self) -> usize {
        ((self.sidebar_rect().h - self.px(SB_TOP)) / self.px(ROW_H)).max(1.0) as usize
    }

    fn sidebar_row_at(&self, y: f32) -> Option<usize> {
        let top = self.px(SB_TOP);
        if y < top || y >= self.sidebar_rect().h {
            return None;
        }
        let n = self.rows().len();
        let i = self.sidebar_top(n) + ((y - top) / self.px(ROW_H)) as usize;
        (i < n).then_some(i)
    }

    pub fn focused(&self) -> Option<u32> {
        let v = self.views.get(self.current.as_ref()?)?;
        v.panes.get(v.focus).copied()
    }

    // ---- panes ----

    fn spawn_pane(&mut self, view: &Path) -> Result<u32, String> {
        let id = self.next_id;
        self.next_id += 1;
        let (ws, proj, wt) = self.names(view);
        let env = [
            ("PMUX", "1".to_string()),
            ("PMUX_SOCKET", self.socket.display().to_string()),
            ("PMUX_PANE", format!("%{id}")),
            ("PMUX_WORKSPACE", ws),
            ("PMUX_PROJECT", proj),
            ("PMUX_WORKTREE", wt),
        ];
        let a = self.pane_area();
        let n = self.panes_in(view).len() + 1;
        let body = R { w: a.w / n as f32 - 2.0 * self.px(PAD), h: a.h - 2.0 * self.px(PAD), ..a };
        let (cols, rows) = self.grid_size(body);
        let pty = pty::spawn(view, rows as u16, cols as u16, &env).map_err(|e| e.to_string())?;
        let mut r = pty.master.try_clone().map_err(|e| e.to_string())?;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send_event(Ev::Output(id, buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = tx.send_event(Ev::Exit(id));
        });
        let mut child = pty.child;
        let pgid = child.id() as i32;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let long_ago = Instant::now() - Duration::from_secs(60);
        self.panes.insert(
            id,
            Pane {
                id,
                vt: Vt::new(cols, rows),
                master: pty.master,
                shell: pgid,
                pgid,
                view: view.to_path_buf(),
                title: None,
                osc_title: None,
                state: None,
                agent: None,
                bell: false,
                last_output: long_ago,
                last_input: long_ago,
                last_probe: long_ago,
                rows,
                cols,
            },
        );
        let v = self.views.entry(view.to_path_buf()).or_default();
        v.panes.push(id);
        v.focus = v.panes.len() - 1;
        self.dirty = true;
        Ok(id)
    }

    fn open_view(&mut self, view: PathBuf) {
        if self.panes_in(&view).is_empty() {
            if let Err(e) = self.spawn_pane(&view) {
                return self.say(format!("spawn failed: {e}"));
            }
        }
        if let Some(i) = self.rows().iter().position(|r| self.row_view(*r).as_ref() == Some(&view)) {
            self.sel = i;
        }
        self.current = Some(view);
        self.sidebar_focus = false;
        self.see_focused();
    }

    /// Looking at a pane clears its bell.
    fn see_focused(&mut self) {
        if let Some(id) = self.focused() {
            self.panes.get_mut(&id).unwrap().bell = false;
        }
    }

    fn say(&mut self, m: String) {
        self.flash = Some((m, Instant::now()));
    }

    fn write_pane(&mut self, id: u32, bytes: &[u8]) {
        if let Some(p) = self.panes.get_mut(&id) {
            p.last_input = Instant::now();
            let _ = p.master.write_all(bytes);
        }
    }

    /// Typing into a pane: jump back to the live screen first.
    fn type_pane(&mut self, id: u32, bytes: &[u8]) {
        if let Some(p) = self.panes.get_mut(&id) {
            if p.vt.display_offset() > 0 {
                p.vt.scroll(Scroll::Bottom);
            }
        }
        self.write_pane(id, bytes);
    }

    fn kill_pane(&mut self, id: u32) {
        if let Some(p) = self.panes.get(&id) {
            unsafe { libc::kill(-p.pgid, libc::SIGHUP) };
            unsafe { libc::kill(-p.shell, libc::SIGHUP) };
        }
    }

    pub fn kill_all(&mut self) {
        let ids: Vec<u32> = self.panes.keys().copied().collect();
        for id in ids {
            self.kill_pane(id);
        }
    }

    pub fn handle(&mut self, ev: Ev) {
        self.dirty = true;
        match ev {
            Ev::Output(id, data) => self.on_output(id, &data),
            Ev::Exit(id) => {
                if let Some(p) = self.panes.remove(&id) {
                    if let Some(v) = self.views.get_mut(&p.view) {
                        v.panes.retain(|x| *x != id);
                        v.focus = v.focus.min(v.panes.len().saturating_sub(1));
                    }
                }
                if self.focused().is_none() {
                    self.sidebar_focus = true;
                }
            }
            Ev::Api(req, rtx) if req.args.first().map(String::as_str) == Some("snapshot") => {
                let path = req.args.get(1).cloned().unwrap_or_else(|| "pmux-snapshot.bmp".into());
                self.snapshot = Some((path, rtx));
            }
            Ev::Api(req, rtx) => {
                let r = self.on_api(req);
                let _ = rtx.send(r);
            }
            Ev::Behind(m) => self.behind = m,
            Ev::Git => {
                self.refresh_git();
                self.rewatch();
            }
        }
    }

    fn on_output(&mut self, id: u32, data: &[u8]) {
        let focused = self.focused();
        let (cw, ch) = (self.cell_w as u16, self.cell_h as u16);
        let Some(p) = self.panes.get_mut(&id) else { return };
        let events = p.vt.feed(data);
        let now = Instant::now();
        if now - p.last_input > Duration::from_millis(300) {
            p.last_output = now;
        }
        for e in events {
            match e {
                TEvent::PtyWrite(s) => {
                    let _ = p.master.write_all(s.as_bytes());
                }
                TEvent::Title(t) => p.osc_title = Some(t),
                TEvent::ResetTitle => p.osc_title = None,
                TEvent::Bell => p.bell = Some(id) != focused,
                TEvent::ColorRequest(i, f) => {
                    let rgb = p.vt.term.colors()[i].unwrap_or_else(|| crate::ui::default_color(i));
                    let _ = p.master.write_all(f(rgb).as_bytes());
                }
                TEvent::TextAreaSizeRequest(f) => {
                    let ws = WindowSize { num_lines: p.rows as u16, num_cols: p.cols as u16, cell_width: cw, cell_height: ch };
                    let _ = p.master.write_all(f(ws).as_bytes());
                }
                TEvent::ClipboardStore(_, s) => clipboard_set(&s),
                _ => {}
            }
        }
        // Agent detection: which process owns the terminal (throttled syscall).
        if now - p.last_probe > Duration::from_millis(250) {
            p.last_probe = now;
            let pg = pty::fg_pgrp(&p.master);
            if pg != p.pgid {
                p.pgid = pg;
                let a = pty::proc_path(pg).as_deref().and_then(pty::agent_of);
                if a != p.agent {
                    p.agent = a;
                    p.state = None;
                }
            }
        }
    }

    // ---- mouse ----

    pub fn on_cursor_moved(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        if self.menu.is_some() {
            let h = self.menu_item_at(x, y);
            let m = self.menu.as_mut().unwrap();
            if m.hover != h {
                m.hover = h;
                self.dirty = true;
            }
            let icon = if h.is_some() { CursorIcon::Pointer } else { CursorIcon::Default };
            if icon != self.cursor_icon {
                self.cursor_icon = icon;
                self.dirty = true;
            }
            return;
        }
        if self.drag_sidebar {
            let max = (self.w / self.scale / 2.0).max(160.0);
            self.sidebar_w = (x / self.scale).clamp(160.0, max);
            self.dirty = true;
            return;
        }
        if let Some((id, b)) = self.mouse_pane {
            return self.pane_mouse(id, MouseKind::Drag(b), x, y);
        }
        if let Some(d) = &self.drag {
            if d.active || (y - d.start_y).abs() > self.px(4.0) {
                let row = d.row;
                let target = if self.sidebar_rect().contains(x, y) { self.drop_target(row, y) } else { None };
                let d = self.drag.as_mut().unwrap();
                d.active = true;
                d.target = target;
                self.hover = None;
                self.hover_btn = 0;
                self.cursor_icon = CursorIcon::Grabbing;
                self.dirty = true;
            }
            return;
        }
        let sb = self.sidebar_rect();
        let edge = (x - sb.w).abs() <= self.px(3.0) && self.show_sidebar;
        let (hover, icon) = if self.prompt.is_some() {
            (None, CursorIcon::Default)
        } else if edge {
            (None, CursorIcon::ColResize)
        } else if sb.contains(x, y) {
            let h = self.sidebar_row_at(y);
            (h, if h.is_some() { CursorIcon::Pointer } else { CursorIcon::Default })
        } else if self.layout().iter().any(|p| p.body.contains(x, y)) {
            (None, CursorIcon::Text)
        } else {
            (None, CursorIcon::Default)
        };
        let btn = if self.prompt.is_some() || !sb.contains(x, y) { 0 } else { self.button_at(hover, x, y) };
        let icon = if btn > 0 { CursorIcon::Pointer } else { icon };
        if hover != self.hover || icon != self.cursor_icon || btn != self.hover_btn {
            self.hover = hover;
            self.hover_btn = btn;
            self.cursor_icon = icon;
            self.dirty = true;
        }
    }

    pub fn on_mouse_button(&mut self, state: ElementState, b: MouseButton) {
        let (x, y) = self.mouse;
        self.dirty = true;
        if state == ElementState::Released {
            self.drag_sidebar = false;
            if let Some((id, mb)) = self.mouse_pane.take() {
                self.pane_mouse(id, MouseKind::Up(mb), x, y);
            }
            if let Some(d) = self.drag.take() {
                match (d.active, d.target) {
                    (false, _) => self.click_row(d.row),
                    (true, Some((t, _, _))) => self.drop_row(d.row, t),
                    _ => {}
                }
                self.on_cursor_moved(x, y);
            }
            return;
        }
        // An open menu takes the click: run the item, or dismiss.
        if self.menu.is_some() {
            let hit = self.menu_item_at(x, y);
            let m = self.menu.take().unwrap();
            if let (Some(i), MouseButton::Left) = (hit, b) {
                self.run_action(m.items[i].1.clone());
            }
            return;
        }
        let sb = self.sidebar_rect();
        if b == MouseButton::Right && sb.contains(x, y) && self.prompt.is_none() {
            return self.open_menu(self.sidebar_row_at(y), x, y);
        }
        if self.show_sidebar && (x - sb.w).abs() <= self.px(3.0) {
            self.drag_sidebar = b == MouseButton::Left;
            return;
        }
        if sb.contains(x, y) {
            if b != MouseButton::Left {
                return;
            }
            let hit = self.sidebar_row_at(y);
            let Some(i) = hit else { return };
            let rows = self.rows();
            let row = rows[i];
            self.sel = i;
            if let (2, Row::Ws(w)) = (self.button_at(Some(i), x, y), row) {
                return self.prompt_add_project(Some(w));
            }
            // Project chevron toggles fold; the rest opens.
            if let Row::Proj(w, p) = row {
                let proj = &self.workspaces[w].projects[p];
                if proj.has_linked() && self.chevron_rect(1, self.row_y(i)).contains(x, y) {
                    let key = proj_key(&proj.path);
                    if !self.folded.remove(&key) {
                        self.folded.insert(key);
                    }
                    return;
                }
            }
            // Workspace/project rows act on release, so they can be dragged to reorder.
            if matches!(row, Row::Ws(_) | Row::Proj(..)) {
                self.drag = Some(Drag { row, start_y: y, active: false, target: None });
                return;
            }
            return self.click_row(row);
        }
        let Some(pr) = self.layout().into_iter().find(|p| p.header.contains(x, y) || p.body.contains(x, y) || R { y: p.header.y, h: self.h, ..p.header }.contains(x, y)) else { return };
        if let Some(v) = self.current.as_ref().and_then(|c| self.views.get_mut(c)) {
            v.focus = v.panes.iter().position(|p| *p == pr.id).unwrap_or(0);
        }
        self.sidebar_focus = false;
        self.see_focused();
        if pr.body.contains(x, y) {
            let mode = self.panes[&pr.id].vt.mode();
            if mode.intersects(TermMode::MOUSE_MODE) {
                self.mouse_pane = Some((pr.id, b));
                self.pane_mouse(pr.id, MouseKind::Down(b), x, y);
            }
        }
    }

    fn click_row(&mut self, row: Row) {
        if !self.row_valid(row) {
            return;
        }
        match self.row_view(row) {
            Some(v) => self.open_view(v),
            None => {
                if let Row::Ws(w) = row {
                    let key = ws_key(&self.workspaces[w].name);
                    if !self.folded.remove(&key) {
                        self.folded.insert(key);
                    }
                }
                self.sidebar_focus = true;
            }
        }
    }

    fn row_valid(&self, row: Row) -> bool {
        match row {
            Row::Ws(w) => w < self.workspaces.len(),
            Row::Proj(w, p) => self.workspaces.get(w).is_some_and(|ws| p < ws.projects.len()),
            Row::Wt(w, p, t) => self.workspaces.get(w).and_then(|ws| ws.projects.get(p)).and_then(|pr| pr.worktrees.as_ref()).is_some_and(|v| t < v.len()),
        }
    }

    /// Drop target for dragging `row` with the mouse at screen y: workspaces reorder among themselves,
    /// projects reorder within or move across workspaces.
    fn drop_target(&self, row: Row, y: f32) -> Option<(Drop, usize, usize)> {
        let rows = self.rows();
        let n = rows.len();
        if n == 0 {
            return None;
        }
        let pos = self.sidebar_top(n) as f32 + (y - self.px(SB_TOP)) / self.px(ROW_H);
        let idx = (pos.floor().max(0.0) as usize).min(n - 1);
        // Row block [start, end): the row plus everything nested under it.
        let block = |start: usize| {
            let lv = App::level(rows[start]);
            (start, rows[start + 1..].iter().position(|r| App::level(*r) <= lv).map_or(n, |k| start + 1 + k))
        };
        let before = |(s, e): (usize, usize)| pos < (s + e) as f32 / 2.0;
        match row {
            Row::Ws(_) => {
                let w = match rows[idx] {
                    Row::Ws(w) | Row::Proj(w, _) | Row::Wt(w, _, _) => w,
                };
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

    fn drop_row(&mut self, row: Row, to: Drop) {
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
                let path = &self.workspaces[fw].projects[fp].path;
                if tw != fw && self.workspaces[tw].projects.iter().any(|p| &p.path == path) {
                    let name = self.workspaces[tw].name.clone();
                    return self.say(format!("already in workspace {name}"));
                }
                let pr = self.workspaces[fw].projects.remove(fp);
                let tp = if tw == fw && tp > fp { tp - 1 } else { tp };
                self.workspaces[tw].projects.insert(tp, pr);
                Row::Proj(tw, tp)
            }
            _ => return,
        };
        if let Some(i) = self.rows().iter().position(|r| *r == moved) {
            self.sel = i;
        }
        self.save();
        self.dirty = true;
    }

    /// Wheel: `dy` in lines (positive = up / back in history).
    pub fn on_wheel(&mut self, dy_lines: f32) {
        let (x, y) = self.mouse;
        self.wheel_acc += dy_lines;
        let n = self.wheel_acc.trunc() as i32;
        if n == 0 {
            return;
        }
        self.wheel_acc -= n as f32;
        self.dirty = true;
        if self.sidebar_rect().contains(x, y) {
            let len = self.rows().len();
            self.sel = (self.sel as i32 - n).clamp(0, len.saturating_sub(1) as i32) as usize;
            return;
        }
        let Some(pr) = self.layout().into_iter().find(|p| p.body.contains(x, y)) else { return };
        let mode = self.panes[&pr.id].vt.mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            for _ in 0..n.abs() {
                self.pane_mouse(pr.id, if n > 0 { MouseKind::WheelUp } else { MouseKind::WheelDown }, x, y);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) {
            if mode.contains(TermMode::ALTERNATE_SCROLL) {
                let k: &[u8] = if n > 0 { b"\x1b[A" } else { b"\x1b[B" };
                let bytes = k.repeat(n.unsigned_abs() as usize);
                self.write_pane(pr.id, &bytes);
            }
        } else if let Some(p) = self.panes.get_mut(&pr.id) {
            p.vt.scroll(Scroll::Delta(n));
        }
    }

    /// Encode a mouse event for the pane's app (xterm / SGR).
    fn pane_mouse(&mut self, id: u32, kind: MouseKind, x: f32, y: f32) {
        let Some(pr) = self.layout().into_iter().find(|p| p.id == id) else { return };
        let Some(p) = self.panes.get(&id) else { return };
        let mode = p.vt.mode();
        let col = (((x - pr.body.x) / self.cell_w).floor().max(0.0) as usize).min(p.cols.saturating_sub(1)) as u32 + 1;
        let row = (((y - pr.body.y) / self.cell_h).floor().max(0.0) as usize).min(p.rows.saturating_sub(1)) as u32 + 1;
        let btn = |b: MouseButton| match b {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => 0,
        };
        let motion = mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION);
        let (code, release) = match kind {
            MouseKind::Down(b) => (btn(b), false),
            MouseKind::Up(b) => (btn(b), true),
            MouseKind::Drag(b) if motion => (btn(b) + 32, false),
            MouseKind::Drag(_) => return,
            MouseKind::WheelUp => (64, false),
            MouseKind::WheelDown => (65, false),
        };
        let mut m = self.mods;
        m.remove(ModifiersState::SUPER);
        let code = code + 4 * m.shift_key() as u32 + 8 * m.alt_key() as u32 + 16 * m.control_key() as u32;
        let bytes = if mode.contains(TermMode::SGR_MOUSE) {
            format!("\x1b[<{code};{col};{row}{}", if release { 'm' } else { 'M' }).into_bytes()
        } else {
            let c = if release { 3 } else { code };
            let mut v = b"\x1b[M".to_vec();
            v.extend([(32 + c).min(255) as u8, (32 + col.min(223)) as u8, (32 + row.min(223)) as u8]);
            v
        };
        let _ = self.panes.get_mut(&id).unwrap().master.write_all(&bytes);
    }

    // ---- keys ----

    pub fn on_paste(&mut self, s: &str) {
        if let Some(p) = &mut self.prompt {
            p.input.push_str(s.trim());
            return;
        }
        if let Some(id) = self.focused().filter(|_| !self.sidebar_focus) {
            let bp = self.panes[&id].vt.mode().contains(TermMode::BRACKETED_PASTE);
            let s = s.replace("\r\n", "\r").replace('\n', "\r");
            let data = if bp { format!("\x1b[200~{s}\x1b[201~") } else { s };
            self.type_pane(id, data.as_bytes());
        }
    }

    pub fn on_key(&mut self, k: &KeyEvent) {
        if k.state != ElementState::Pressed {
            return;
        }
        self.dirty = true;
        let m = self.mods;
        let ch = match &k.logical_key {
            Key::Character(s) => s.chars().next(),
            _ => None,
        };
        if self.menu.is_some() {
            return self.menu_key(k);
        }
        // App shortcuts on Cmd (Super).
        if m.super_key() {
            return self.cmd_key(k, ch);
        }
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        let ctrl_a = m.control_key() && input::base_char(k, ModifiersState::empty()) == Some('a');
        if self.prefix {
            self.prefix = false;
            return self.prefix_key(k, ch, ctrl_a);
        }
        if ctrl_a {
            self.prefix = true;
            return;
        }
        if self.sidebar_focus || self.focused().is_none() {
            return self.sidebar_key(k, ch);
        }
        let id = self.focused().unwrap();
        let ac = self.panes[&id].vt.mode().contains(TermMode::APP_CURSOR);
        let bytes = input::encode(k, m, ac);
        if !bytes.is_empty() {
            self.type_pane(id, &bytes);
        }
    }

    fn cmd_key(&mut self, k: &KeyEvent, ch: Option<char>) {
        match (&k.logical_key, ch.map(|c| c.to_ascii_lowercase())) {
            (_, Some('k')) => self.clear_focused(),
            (_, Some('v')) => {
                if let Some(s) = clipboard_get() {
                    self.on_paste(&s);
                }
            }
            (_, Some('d')) => self.split(),
            (_, Some('w')) => {
                if let Some(id) = self.focused() {
                    self.kill_pane(id);
                }
            }
            (_, Some('[')) => self.move_focus(-1),
            (_, Some(']')) => self.move_focus(1),
            (_, Some('b')) => self.show_sidebar = !self.show_sidebar,
            (_, Some('e')) => self.sidebar_focus = !self.sidebar_focus || self.focused().is_none(),
            (_, Some('n')) => self.next_attention(),
            (_, Some('q')) => self.quit = true,
            (_, Some('=' | '+')) => self.zoom = Some(1),
            (_, Some('-')) => self.zoom = Some(-1),
            (_, Some('0')) => self.zoom = Some(0),
            (Key::Named(NamedKey::ArrowDown), _) => self.step_view(1),
            (Key::Named(NamedKey::ArrowUp), _) => self.step_view(-1),
            _ => {}
        }
    }

    fn prefix_key(&mut self, k: &KeyEvent, ch: Option<char>, ctrl_a: bool) {
        if ctrl_a {
            if let Some(id) = self.focused() {
                self.type_pane(id, &[1]);
            }
            return;
        }
        match (&k.logical_key, ch) {
            (_, Some('|' | '%' | '\\')) => self.split(),
            (Key::Named(NamedKey::ArrowLeft), _) | (_, Some('h')) => self.move_focus(-1),
            (Key::Named(NamedKey::ArrowRight), _) | (_, Some('l')) => self.move_focus(1),
            (_, Some('x')) => {
                if let Some(id) = self.focused() {
                    self.kill_pane(id);
                }
            }
            (Key::Named(NamedKey::Tab), _) | (_, Some('s')) => self.sidebar_focus = !self.sidebar_focus || self.focused().is_none(),
            (_, Some('b')) => self.show_sidebar = !self.show_sidebar,
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some('j')) => self.step_view(1),
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some('k')) => self.step_view(-1),
            (_, Some('n')) => self.next_attention(),
            (_, Some('c')) => self.clear_focused(),
            (_, Some('q')) => self.quit = true,
            _ => {}
        }
    }

    fn split(&mut self) {
        if let Some(v) = self.current.clone() {
            if let Err(e) = self.spawn_pane(&v) {
                self.say(e);
            }
            self.sidebar_focus = false;
        }
    }

    /// Clear only this pane: wipe its screen + scrollback, then let the app redraw (Ctrl-L).
    fn clear_focused(&mut self) {
        if let Some(id) = self.focused() {
            self.panes.get_mut(&id).unwrap().vt.clear();
            self.write_pane(id, &[0x0c]);
        }
    }

    fn next_attention(&mut self) {
        let mut ids: Vec<_> = self.panes.values().filter(|p| p.state() == Some("waiting")).map(|p| (p.id, p.view.clone())).collect();
        ids.sort();
        if let Some((id, v)) = ids.into_iter().next() {
            self.open_view(v.clone());
            let view = self.views.get_mut(&v).unwrap();
            view.focus = view.panes.iter().position(|x| *x == id).unwrap_or(0);
            self.see_focused();
        }
    }

    fn move_focus(&mut self, d: isize) {
        if let Some(v) = self.current.as_ref().and_then(|c| self.views.get_mut(c)) {
            let n = v.panes.len() as isize;
            if n > 0 {
                v.focus = ((v.focus as isize + d).rem_euclid(n)) as usize;
            }
        }
        self.sidebar_focus = false;
        self.see_focused();
    }

    fn step_view(&mut self, d: isize) {
        let views: Vec<PathBuf> = self.rows().into_iter().filter_map(|r| self.row_view(r)).collect();
        if views.is_empty() {
            return;
        }
        let i = self.current.as_ref().and_then(|c| views.iter().position(|v| v == c)).map(|i| i as isize).unwrap_or(-1);
        let n = views.len() as isize;
        self.open_view(views[((i + d).rem_euclid(n)) as usize].clone());
    }

    fn sidebar_key(&mut self, k: &KeyEvent, ch: Option<char>) {
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
        let wi = match row {
            Row::Ws(w) | Row::Proj(w, _) | Row::Wt(w, _, _) => w,
        };
        let fold_key = match row {
            Row::Ws(w) => Some(ws_key(&self.workspaces[w].name)),
            Row::Proj(w, p) if self.workspaces[w].projects[p].has_linked() => Some(proj_key(&self.workspaces[w].projects[p].path)),
            _ => None,
        };
        let named = match &k.logical_key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        let down = named == Some(NamedKey::ArrowDown) || ch == Some('j');
        let up = named == Some(NamedKey::ArrowUp) || ch == Some('k');
        let right = named == Some(NamedKey::ArrowRight) || ch == Some('l');
        let left = named == Some(NamedKey::ArrowLeft) || ch == Some('h');
        let enter = named == Some(NamedKey::Enter);
        if down {
            self.sel = (self.sel + 1).min(rows.len() - 1);
        } else if up {
            self.sel = self.sel.saturating_sub(1);
        } else if right && fold_key.as_ref().is_some_and(|f| self.folded.contains(f)) {
            self.folded.remove(fold_key.as_ref().unwrap());
        } else if enter || right {
            match self.row_view(row) {
                Some(v) => self.open_view(v),
                None => {
                    if let Some(f) = fold_key {
                        if !self.folded.remove(&f) && enter {
                            self.folded.insert(f);
                        }
                    }
                }
            }
        } else if left {
            match (row, fold_key) {
                (_, Some(f)) if !self.folded.contains(&f) => {
                    self.folded.insert(f);
                }
                (Row::Proj(w, _), _) => self.sel = rows.iter().position(|r| *r == Row::Ws(w)).unwrap_or(0),
                (Row::Wt(w, p, _), _) => self.sel = rows.iter().position(|r| *r == Row::Proj(w, p)).unwrap_or(0),
                _ => {}
            }
        } else if named == Some(NamedKey::Escape) || named == Some(NamedKey::Tab) {
            if self.focused().is_some() {
                self.sidebar_focus = false;
            }
        } else {
            match ch {
                Some('a') => self.prompt_add_project(Some(wi)),
                Some('W') => self.prompt_new_workspace(),
                Some('w') => match row {
                    Row::Proj(w, p) | Row::Wt(w, p, _) if self.workspaces[w].projects[p].worktrees.is_some() => {
                        let path = self.workspaces[w].projects[p].path.clone();
                        self.prompt = Some(Prompt::new(PromptKind::NewWorktree(path), "New worktree branch", ""));
                    }
                    _ => self.say("not a git project".into()),
                },
                Some('x') => match row {
                    Row::Ws(w) => self.request_close(Close::Workspace(w)),
                    Row::Proj(w, p) => self.request_close(Close::Project(w, p)),
                    Row::Wt(..) => self.say("x closes a project or workspace; D removes a worktree".into()),
                },
                Some('D') => {
                    if let Row::Wt(w, p, t) = row {
                        self.delete_worktree(w, p, t);
                    }
                }
                Some('q') => self.quit = true,
                _ => {}
            }
        }
    }

    fn prompt_key(&mut self, k: &KeyEvent) {
        let Some(mut p) = self.prompt.take() else { return };
        if let PromptKind::Confirm(c) = &p.kind {
            let yes = matches!(&k.logical_key, Key::Named(NamedKey::Enter)) || matches!(&k.logical_key, Key::Character(s) if s.as_str() == "y");
            if yes {
                self.close(c.clone());
            }
            return;
        }
        match &k.logical_key {
            Key::Named(NamedKey::Escape) => {}
            Key::Named(NamedKey::Enter) => self.submit(p),
            Key::Named(NamedKey::Backspace) => {
                p.input.pop();
                self.prompt = Some(p);
            }
            Key::Named(NamedKey::Tab) => {
                if matches!(p.kind, PromptKind::AddProject(_)) {
                    p.input = complete(&p.input);
                }
                self.prompt = Some(p);
            }
            _ => {
                if let Some(t) = &k.text {
                    p.input.extend(t.chars().filter(|c| !c.is_control()));
                }
                self.prompt = Some(p);
            }
        }
    }

    fn submit(&mut self, p: Prompt) {
        let input = p.input.trim().to_string();
        if input.is_empty() {
            return;
        }
        match p.kind {
            PromptKind::AddProject(wi) => match self.add_project(wi, &input) {
                Ok(name) => self.say(format!("added {name}")),
                Err(e) => self.say(e),
            },
            PromptKind::NewWorkspace => {
                if let Err(e) = self.add_workspace(&input) {
                    self.say(e);
                }
            }
            PromptKind::Confirm(c) => self.close(c),
            PromptKind::RenameWorkspace(w) => {
                if let Err(e) = self.rename_workspace(w, &input) {
                    self.say(e);
                }
            }
            PromptKind::NewWorktree(proj) => match git::add(&proj, &input) {
                Ok(dest) => {
                    self.refresh_git();
                    self.rewatch();
                    self.open_view(dest);
                }
                Err(e) => self.say(e),
            },
        }
    }

    // ---- closing workspaces / projects (sidebar only: files are never touched) ----

    fn close_paths(&self, c: &Close) -> (String, Vec<PathBuf>) {
        match *c {
            Close::Workspace(w) => {
                let ws = &self.workspaces[w];
                (format!("workspace {}", ws.name), ws.projects.iter().flat_map(|p| p.view_paths()).collect())
            }
            Close::Project(w, p) => {
                let pr = &self.workspaces[w].projects[p];
                (format!("project {}", pr.name), pr.view_paths())
            }
            Close::Worktree(_, ref wt, _) => {
                let (_, _, b) = self.names(wt);
                (format!("worktree {b}"), vec![wt.clone()])
            }
        }
    }

    /// Close right away if nothing runs inside, otherwise ask first.
    pub fn request_close(&mut self, c: Close) {
        let (name, paths) = self.close_paths(&c);
        let n: usize = paths.iter().map(|v| self.panes_in(v).len()).sum();
        if let Close::Worktree(proj, wt, _) = &c {
            // Deletes files on disk: always ask. Say what else goes with it.
            let dirty = git::dirty(wt);
            let panes = if n > 0 { format!(" and close its {n} pane{}", if n == 1 { "" } else { "s" }) } else { String::new() };
            let lost = if dirty { " It has uncommitted changes, which will be lost." } else { "" };
            let label = format!("Delete {name}{panes}? Removes its folder.{lost} y/n");
            let c = Close::Worktree(proj.clone(), wt.clone(), dirty);
            self.prompt = Some(Prompt { kind: PromptKind::Confirm(c), label, input: String::new() });
            return;
        }
        if n == 0 {
            return self.close(c);
        }
        let label = format!("Delete {name} and close its {n} pane{}? Files stay on disk. y/n", if n == 1 { "" } else { "s" });
        self.prompt = Some(Prompt { kind: PromptKind::Confirm(c), label, input: String::new() });
    }

    fn close(&mut self, c: Close) {
        let (name, paths) = self.close_paths(&c);
        for v in &paths {
            for id in self.panes_in(v).to_vec() {
                self.kill_pane(id);
            }
        }
        if self.current.as_ref().is_some_and(|cur| paths.contains(cur)) {
            self.current = None;
            self.sidebar_focus = true;
        }
        if let Close::Worktree(proj, wt, force) = &c {
            let r = git::remove(proj, wt, *force);
            self.say(match r {
                Ok(()) => format!("deleted {name}"),
                Err(e) => format!("couldn't delete {name}: {e}"),
            });
            self.refresh_git();
            self.rewatch();
            self.sel = self.sel.min(self.rows().len().saturating_sub(1));
            self.dirty = true;
            return;
        }
        match c {
            Close::Workspace(w) => {
                let ws = self.workspaces.remove(w);
                self.folded.remove(&ws_key(&ws.name));
            }
            Close::Project(w, p) => {
                let pr = self.workspaces[w].projects.remove(p);
                self.folded.remove(&proj_key(&pr.path));
            }
            Close::Worktree(..) => unreachable!(),
        }
        self.sel = self.sel.min(self.rows().len().saturating_sub(1));
        self.save();
        self.rewatch();
        self.say(format!("deleted {name} from pmux"));
        self.dirty = true;
    }

    fn api_close(&mut self, c: Close, force: bool) -> Reply {
        let (name, paths) = self.close_paths(&c);
        let n: usize = paths.iter().map(|v| self.panes_in(v).len()).sum();
        if n > 0 && !force {
            return Reply::err(format!("{name} has {n} open pane(s); add --force to close them"));
        }
        self.close(c);
        Reply::ok(format!("closed {name}"))
    }

    // ---- context menu ----

    fn open_menu(&mut self, row: Option<usize>, x: f32, y: f32) {
        let rows = self.rows();
        let fold = |app: &Self, key: String| -> (String, Action) {
            let label = if app.folded.contains(&key) { "Expand" } else { "Collapse" };
            (label.into(), Action::Toggle(key))
        };
        let items: Vec<(String, Action)> = match row.and_then(|i| rows.get(i).copied()) {
            Some(Row::Ws(w)) => vec![
                ("New project…".into(), Action::AddProject(w)),
                ("Rename…".into(), Action::RenameWorkspace(w)),
                fold(self, ws_key(&self.workspaces[w].name)),
                (String::new(), Action::Sep),
                ("Delete workspace".into(), Action::Delete(Close::Workspace(w))),
            ],
            Some(Row::Proj(w, p)) => {
                let pr = &self.workspaces[w].projects[p];
                let mut v = vec![("Open".into(), Action::Open(pr.path.clone()))];
                if pr.worktrees.is_some() {
                    v.push(("New worktree…".into(), Action::NewWorktree(pr.path.clone())));
                }
                if pr.has_linked() {
                    v.push(fold(self, proj_key(&pr.path)));
                }
                v.push((String::new(), Action::Sep));
                v.push(("Copy path".into(), Action::CopyPath(pr.path.clone())));
                v.push((REVEAL.into(), Action::Reveal(pr.path.clone())));
                v.push((String::new(), Action::Sep));
                v.push(("Delete project".into(), Action::Delete(Close::Project(w, p))));
                v
            }
            Some(Row::Wt(w, p, t)) => {
                let pr = &self.workspaces[w].projects[p];
                let wt = &pr.worktrees.as_ref().unwrap()[t];
                let mut v = vec![
                    ("Open".into(), Action::Open(wt.path.clone())),
                    (String::new(), Action::Sep),
                    ("Copy path".into(), Action::CopyPath(wt.path.clone())),
                    (REVEAL.into(), Action::Reveal(wt.path.clone())),
                ];
                if !wt.main {
                    v.push((String::new(), Action::Sep));
                    v.push(("Delete worktree".into(), Action::Delete(Close::Worktree(pr.path.clone(), wt.path.clone(), false))));
                }
                v
            }
            None => vec![("New workspace…".into(), Action::NewWorkspace), ("Add project…".into(), Action::AddProject(usize::MAX))],
        };
        if let Some(i) = row {
            self.sel = i;
        }
        let (w, h) = self.menu_size(&items);
        let x = x.min(self.w - w - self.px(4.0)).max(0.0);
        let y = y.min(self.h - h - self.px(4.0)).max(0.0);
        self.menu = Some(Menu { x, y, items, hover: None });
        self.dirty = true;
    }

    pub fn menu_size(&self, items: &[(String, Action)]) -> (f32, f32) {
        let h: f32 = items.iter().map(|(_, a)| if matches!(a, Action::Sep) { self.px(9.0) } else { self.px(26.0) }).sum();
        (self.px(210.0), h + 2.0 * self.px(4.0))
    }

    /// Item rects of the open menu (separators included, as thin rows).
    pub fn menu_rects(&self) -> Vec<R> {
        let Some(m) = &self.menu else { return vec![] };
        let (w, _) = self.menu_size(&m.items);
        let pad = self.px(4.0);
        let mut y = m.y + pad;
        m.items
            .iter()
            .map(|(_, a)| {
                let h = if matches!(a, Action::Sep) { self.px(9.0) } else { self.px(26.0) };
                let r = R { x: m.x + pad, y, w: w - 2.0 * pad, h };
                y += h;
                r
            })
            .collect()
    }

    fn menu_item_at(&self, x: f32, y: f32) -> Option<usize> {
        let m = self.menu.as_ref()?;
        self.menu_rects().iter().position(|r| r.contains(x, y)).filter(|i| !matches!(m.items[*i].1, Action::Sep))
    }

    fn menu_key(&mut self, k: &KeyEvent) {
        let Some(m) = self.menu.as_mut() else { return };
        let n = m.items.len();
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
        match &k.logical_key {
            Key::Named(NamedKey::ArrowDown) => m.hover = step(m.hover, 1),
            Key::Named(NamedKey::ArrowUp) => m.hover = step(m.hover, -1),
            Key::Named(NamedKey::Enter) => {
                if let Some(i) = m.hover {
                    let a = m.items[i].1.clone();
                    self.menu = None;
                    self.run_action(a);
                }
            }
            Key::Named(NamedKey::Escape) => self.menu = None,
            _ => {}
        }
    }

    fn run_action(&mut self, a: Action) {
        self.dirty = true;
        match a {
            Action::AddProject(w) => self.prompt_add_project((w != usize::MAX).then_some(w)),
            Action::NewWorkspace => self.prompt_new_workspace(),
            Action::RenameWorkspace(w) => {
                let name = self.workspaces[w].name.clone();
                self.prompt = Some(Prompt::new(PromptKind::RenameWorkspace(w), "Rename workspace", &name));
            }
            Action::Toggle(key) => {
                if !self.folded.remove(&key) {
                    self.folded.insert(key);
                }
            }
            Action::NewWorktree(p) => self.prompt = Some(Prompt::new(PromptKind::NewWorktree(p), "New worktree branch", "")),
            Action::Open(v) => self.open_view(v),
            Action::CopyPath(p) => {
                clipboard_set(&p.display().to_string());
                self.say(format!("copied {}", config::tilde(&p)));
            }
            Action::Reveal(p) => {
                let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                let _ = std::process::Command::new(cmd).arg(&p).spawn();
            }
            Action::Delete(c) => self.request_close(c),
            Action::Sep => {}
        }
    }

    fn rename_workspace(&mut self, w: usize, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("workspace name is empty".into());
        }
        if self.workspaces.iter().enumerate().any(|(i, ws)| i != w && ws.name == name) {
            return Err(format!("workspace {name} already exists"));
        }
        let old = std::mem::replace(&mut self.workspaces[w].name, name.into());
        if self.folded.remove(&ws_key(&old)) {
            self.folded.insert(ws_key(name));
        }
        self.save();
        Ok(())
    }

    fn delete_worktree(&mut self, w: usize, p: usize, t: usize) {
        let pr = &self.workspaces[w].projects[p];
        let wt = pr.worktrees.as_ref().unwrap()[t].clone();
        if wt.main {
            return self.say("can't delete the main checkout".into());
        }
        self.request_close(Close::Worktree(pr.path.clone(), wt.path, false));
    }

    // ---- adding workspaces / projects ----

    fn prompt_new_workspace(&mut self) {
        self.prompt = Some(Prompt::new(PromptKind::NewWorkspace, "New workspace", ""));
    }

    /// Add a project to workspace `w` (default: the selected row's workspace, else the last one).
    fn prompt_add_project(&mut self, w: Option<usize>) {
        let w = w.or_else(|| {
            self.rows().get(self.sel).map(|r| match *r {
                Row::Ws(w) | Row::Proj(w, _) | Row::Wt(w, _, _) => w,
            })
        });
        let w = match w.or(self.workspaces.len().checked_sub(1)) {
            Some(w) => w,
            None => {
                // No workspace yet: make one so the project has a home.
                self.workspaces.push(Workspace { name: "default".into(), projects: vec![] });
                self.save();
                0
            }
        };
        let label = format!("Add project to {}", self.workspaces[w].name);
        self.prompt = Some(Prompt { kind: PromptKind::AddProject(w), label, input: "~/".into() });
    }

    fn add_workspace(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("workspace name is empty".into());
        }
        if self.workspaces.iter().any(|w| w.name == name) {
            return Err(format!("workspace {name} already exists"));
        }
        self.workspaces.push(Workspace { name: name.into(), projects: vec![] });
        self.sel = self.rows().len() - 1;
        self.save();
        self.dirty = true;
        Ok(())
    }

    fn add_project(&mut self, w: usize, dir: &str) -> Result<String, String> {
        let Some(proj) = make_project(&config::expand(dir.trim())) else { return Err(format!("not a directory: {dir}")) };
        if w >= self.workspaces.len() {
            return Err("no such workspace".into());
        }
        if let Some(ws) = self.workspaces.iter().find(|ws| ws.projects.iter().any(|p| p.path == proj.path)) {
            return Err(format!("{} is already in workspace {}", proj.name, ws.name));
        }
        let name = proj.name.clone();
        self.workspaces[w].projects.push(proj);
        self.folded.remove(&ws_key(&self.workspaces[w].name));
        if let Some(i) = self.rows().iter().position(|r| *r == Row::Proj(w, self.workspaces[w].projects.len() - 1)) {
            self.sel = i;
        }
        self.save();
        self.rewatch();
        self.dirty = true;
        Ok(name)
    }

    /// Which sidebar button is at (x, y), given the row under it.
    fn button_at(&self, row: Option<usize>, x: f32, y: f32) -> u8 {
        let Some(i) = row else { return 0 };
        match self.rows().get(i) {
            Some(Row::Ws(_)) if self.plus_button(self.row_y(i)).contains(x, y) => 2,
            _ => 0,
        }
    }

    /// The hover + button on a workspace row (add project), at the row's right end.
    pub fn plus_button(&self, row_y: f32) -> R {
        self.close_button(row_y)
    }

    /// Tree depth of a sidebar row: workspace 0, project 1, worktree 2.
    pub fn level(r: Row) -> usize {
        match r {
            Row::Ws(_) => 0,
            Row::Proj(..) => 1,
            Row::Wt(..) => 2,
        }
    }

    /// The icon box (16×row) for a row at `level`; its center is where that level's guide line runs.
    /// Clicking a project's folder icon toggles it open/closed.
    pub fn chevron_rect(&self, level: usize, row_y: f32) -> R {
        let x = self.px(ROW_INSET) + self.px(4.0) + level as f32 * self.px(INDENT);
        R { x, y: row_y, w: self.px(16.0), h: self.px(ROW_H) }
    }

    /// The hover × button on a workspace/project row at screen y `row_y`.
    pub fn close_button(&self, row_y: f32) -> R {
        let sb = self.sidebar_rect();
        let d = self.px(18.0);
        R { x: sb.w - self.px(ROW_INSET) - self.px(3.0) - d, y: row_y + ((self.px(ROW_H) - d) / 2.0).round(), w: d, h: d }
    }

    pub fn row_y(&self, i: usize) -> f32 {
        self.px(SB_TOP) + (i - self.sidebar_top(self.rows().len())) as f32 * self.px(ROW_H)
    }

    // ---- socket API ----

    fn resolve(&self, r: &str, caller: Option<&str>) -> Option<u32> {
        let r = if r == "self" { caller? } else { r };
        if let Ok(n) = r.trim_start_matches('%').parse::<u32>() {
            return self.panes.contains_key(&n).then_some(n);
        }
        self.panes.values().find(|p| p.label() == r).map(|p| p.id)
    }

    fn pane_line(&self, p: &Pane, caller: Option<u32>) -> String {
        let (ws, proj, wt) = self.names(&p.view);
        let here = if Some(p.id) == caller { "  ◀ here" } else { "" };
        format!("%{:<3} {:<8} {:<8} {:<36} {}{}", p.id, p.agent.unwrap_or("shell"), p.state().unwrap_or("-"), format!("{ws}/{proj}/{wt}"), p.label(), here)
    }

    fn on_api(&mut self, req: Request) -> Reply {
        let caller = req.caller.as_deref();
        let me = caller.and_then(|c| self.resolve(c, None));
        let a: Vec<&str> = req.args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["ls", ..] => {
                let mut ps: Vec<&Pane> = self.panes.values().collect();
                ps.sort_by_key(|p| p.id);
                Reply::ok(ps.iter().map(|p| self.pane_line(p, me) + "\n").collect::<String>())
            }
            ["whoami", ..] => match me.and_then(|id| self.panes.get(&id)) {
                Some(p) => Reply::ok(self.pane_line(p, None)),
                None => Reply::err("not inside a pmux pane"),
            },
            ["tree", ..] => {
                let mut s = String::new();
                for ws in &self.workspaces {
                    s += &format!("{}\n", ws.name);
                    for p in &ws.projects {
                        s += &format!("  {}  {}\n", p.name, config::tilde(&p.path));
                        let mut views: Vec<(String, PathBuf)> = p.listed().map(|(_, t)| (t.branch.clone(), t.path.clone())).collect();
                        if !views.iter().any(|(_, v)| *v == p.path) {
                            views.insert(0, (String::new(), p.path.clone()));
                        }
                        for (branch, path) in views {
                            let ind = if branch.is_empty() {
                                "    "
                            } else {
                                s += &format!("    {}  {}\n", branch, config::tilde(&path));
                                "      "
                            };
                            for id in self.panes_in(&path) {
                                let pn = &self.panes[id];
                                let here = if Some(*id) == me { "  ◀ here" } else { "" };
                                s += &format!("{ind}%{} {} [{}] {}{}\n", id, pn.agent.unwrap_or("shell"), pn.state().unwrap_or("-"), pn.label(), here);
                            }
                        }
                    }
                }
                Reply::ok(s)
            }
            ["read", t, rest @ ..] => {
                let Some(id) = self.resolve(t, caller) else { return Reply::err(format!("no pane {t}")) };
                let n: usize = rest.first().and_then(|n| n.parse().ok()).unwrap_or(usize::MAX);
                let text = self.panes[&id].vt.text();
                let lines: Vec<&str> = text.trim_end().lines().collect();
                Reply::ok(lines[lines.len().saturating_sub(n)..].join("\n"))
            }
            ["send", t, text @ ..] if !text.is_empty() => {
                let Some(id) = self.resolve(t, caller) else { return Reply::err(format!("no pane {t}")) };
                self.write_pane(id, text.join(" ").as_bytes());
                // Enter separately, so agents don't treat it as part of a paste.
                if let Ok(mut f) = self.panes[&id].master.try_clone() {
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(80));
                        let _ = f.write_all(b"\r");
                    });
                }
                Reply::ok("")
            }
            ["new", cmd @ ..] => {
                let view = me.and_then(|id| self.panes.get(&id)).map(|p| p.view.clone()).or(self.current.clone());
                let Some(view) = view else { return Reply::err("no current worktree") };
                match self.spawn_pane(&view) {
                    Ok(id) => {
                        if !cmd.is_empty() {
                            self.write_pane(id, format!("{}\r", cmd.join(" ")).as_bytes());
                        }
                        Reply::ok(format!("%{id}"))
                    }
                    Err(e) => Reply::err(e),
                }
            }
            ["status", st, rest @ ..] => {
                let Some(id) = rest.first().and_then(|t| self.resolve(t, caller)).or(me) else { return Reply::err("no target pane") };
                let ok = ["working", "waiting", "idle", "done", "error", "clear"];
                if !ok.contains(st) {
                    return Reply::err(format!("state must be one of {}", ok.join("|")));
                }
                self.panes.get_mut(&id).unwrap().state = (*st != "clear").then(|| st.to_string());
                Reply::ok("")
            }
            ["title", text @ ..] if !text.is_empty() => {
                let Some(id) = me else { return Reply::err("not inside a pmux pane") };
                self.panes.get_mut(&id).unwrap().title = Some(text.join(" "));
                Reply::ok("")
            }
            ["ws", "new", name @ ..] if !name.is_empty() => match self.add_workspace(&name.join(" ")) {
                Ok(()) => Reply::ok(""),
                Err(e) => Reply::err(e),
            },
            ["project", "add", dir, rest @ ..] => {
                let w = match rest {
                    ["-w", ws, ..] => self.workspaces.iter().position(|w| w.name == *ws),
                    _ => me.and_then(|id| self.panes.get(&id)).and_then(|p| {
                        let ws = self.names(&p.view).0;
                        self.workspaces.iter().position(|w| w.name == ws)
                    }).or(self.workspaces.len().checked_sub(1)),
                };
                let Some(w) = w else { return Reply::err("no such workspace (create one: pmux ws new <name>)") };
                match self.add_project(w, dir) {
                    Ok(name) => Reply::ok(format!("added {name} to {}", self.workspaces[w].name)),
                    Err(e) => Reply::err(e),
                }
            }
            ["worktree", "rm", t, rest @ ..] => {
                let path = std::fs::canonicalize(config::expand(t)).ok();
                let hit = self.workspaces.iter().flat_map(|w| &w.projects).find_map(|p| {
                    p.worktrees.iter().flatten().find(|w| !w.main && Some(&w.path) == path.as_ref()).map(|w| (p.path.clone(), w.path.clone()))
                });
                let Some((proj, wt)) = hit else { return Reply::err(format!("no linked worktree at {t}")) };
                let force = rest.contains(&"--force");
                let n = self.panes_in(&wt).len();
                if (n > 0 || git::dirty(&wt)) && !force {
                    return Reply::err(format!("worktree has {n} open pane(s) or uncommitted changes; add --force"));
                }
                self.close(Close::Worktree(proj, wt.clone(), force));
                if wt.exists() { Reply::err(self.flash.as_ref().map(|f| f.0.clone()).unwrap_or_default()) } else { Reply::ok(format!("deleted {}", config::tilde(&wt))) }
            }
            ["ws", "rm", name, rest @ ..] => match self.workspaces.iter().position(|w| w.name == *name) {
                Some(w) => self.api_close(Close::Workspace(w), rest.contains(&"--force")),
                None => Reply::err(format!("no workspace {name}")),
            },
            ["project", "rm", t, rest @ ..] => {
                let path = std::fs::canonicalize(config::expand(t)).ok();
                let hit = self.workspaces.iter().enumerate().find_map(|(w, ws)| {
                    ws.projects.iter().position(|p| Some(&p.path) == path.as_ref() || p.name == *t).map(|p| (w, p))
                });
                match hit {
                    Some((w, p)) => self.api_close(Close::Project(w, p), rest.contains(&"--force")),
                    None => Reply::err(format!("no project {t}")),
                }
            }
            _ => Reply::err(format!("unknown command: {} (see pmux --help)", a.join(" "))),
        }
    }

    /// When a time-based state changes next (working → idle heuristic, flash expiry). No idle ticks.
    pub fn next_deadline(&self) -> Option<Instant> {
        let mut d = self
            .panes
            .values()
            .filter(|p| p.agent.is_some() && p.state.is_none() && p.last_output.elapsed() < WORKING_QUIET)
            .map(|p| p.last_output + WORKING_QUIET)
            .min();
        if let Some((_, t)) = &self.flash {
            let f = *t + Duration::from_secs(4);
            if f > Instant::now() {
                d = Some(d.map_or(f, |d| d.min(f)));
            }
        }
        d
    }

    /// Open the cwd's view on launch, if it's in the sidebar.
    pub fn open_cwd(&mut self) {
        if let Ok(cwd) = std::env::current_dir().and_then(std::fs::canonicalize) {
            if self.rows().iter().any(|r| self.row_view(*r).as_ref() == Some(&cwd)) {
                self.open_view(cwd);
            }
        }
    }
}

fn make_project(path: &Path) -> Option<Project> {
    let path = std::fs::canonicalize(path).ok().filter(|p| p.is_dir())?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    let worktrees = git::worktrees(&path);
    Some(Project { path, name, worktrees })
}

const REVEAL: &str = if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Open folder" };

fn complete(input: &str) -> String {
    let full = config::expand(input);
    let (dir, pre) = if input.ends_with('/') {
        (full.clone(), String::new())
    } else {
        (full.parent().map(Path::to_path_buf).unwrap_or_default(), full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
    };
    let Ok(rd) = std::fs::read_dir(&dir) else { return input.into() };
    let m: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&pre) && (!n.starts_with('.') || pre.starts_with('.')))
        .collect();
    let Some(first) = m.first() else { return input.into() };
    let mut common = first.clone();
    for n in &m[1..] {
        while !n.starts_with(&common) {
            common.pop();
        }
    }
    let base = &input[..input.len() - pre.len()];
    format!("{base}{common}{}", if m.len() == 1 { "/" } else { "" })
}

// Clipboard via the platform's CLI tools (prototype; no extra crates).
fn clipboard_get() -> Option<String> {
    let cmds: &[&[&str]] = if cfg!(target_os = "macos") { &[&["pbpaste"]] } else { &[&["wl-paste", "-n"], &["xclip", "-o", "-selection", "clipboard"]] };
    cmds.iter().find_map(|c| {
        let o = std::process::Command::new(c[0]).args(&c[1..]).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    })
}

fn clipboard_set(s: &str) {
    let cmds: &[&[&str]] = if cfg!(target_os = "macos") { &[&["pbcopy"]] } else { &[&["wl-copy"], &["xclip", "-selection", "clipboard"]] };
    for c in cmds {
        if let Ok(mut ch) = std::process::Command::new(c[0]).args(&c[1..]).stdin(std::process::Stdio::piped()).spawn() {
            if let Some(mut i) = ch.stdin.take() {
                let _ = i.write_all(s.as_bytes());
            }
            let _ = ch.wait();
            return;
        }
    }
}
