use crate::api::{Reply, Request};
use crate::{config, git, keys, pty, render, watch};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use crossterm::{execute, terminal};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const WORKING_QUIET: Duration = Duration::from_millis(1500);

pub enum Ev {
    Input(Event),
    Output(u32, Vec<u8>),
    Exit(u32),
    Api(Request, Sender<Reply>),
    Git,
}

/// Answers terminal queries apps send (cursor position, device attributes, colors).
#[derive(Default)]
pub struct Cb {
    pub reply: Vec<u8>,
    pub title: Option<String>,
}

impl vt100::Callbacks for Cb {
    fn unhandled_csi(&mut self, s: &mut vt100::Screen, i1: Option<u8>, _: Option<u8>, params: &[&[u16]], c: char) {
        let p0 = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c) {
            (None, 'n') if p0 == 6 => {
                let (r, c) = s.cursor_position();
                self.reply.extend(format!("\x1b[{};{}R", r + 1, c + 1).bytes());
            }
            (None, 'n') if p0 == 5 => self.reply.extend(b"\x1b[0n"),
            (None, 'c') => self.reply.extend(b"\x1b[?62;22c"),
            (Some(b'>'), 'c') => self.reply.extend(b"\x1b[>0;10;1c"),
            _ => {}
        }
    }
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"10", b"?"] => self.reply.extend(b"\x1b]10;rgb:dddd/dddd/dddd\x1b\\"),
            [b"11", b"?"] => self.reply.extend(b"\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\"),
            _ => {}
        }
    }
    fn set_window_title(&mut self, _: &mut vt100::Screen, t: &[u8]) {
        self.title = Some(String::from_utf8_lossy(t).into_owned());
    }
}

pub struct Pane {
    pub id: u32,
    pub parser: vt100::Parser<Cb>,
    master: File,
    shell: i32,
    pgid: i32,
    pub view: PathBuf,
    pub title: Option<String>,
    pub state: Option<String>,
    pub agent: Option<&'static str>,
    last_output: Instant,
    last_input: Instant,
    last_probe: Instant,
    pub rows: u16,
    pub cols: u16,
}

impl Pane {
    pub fn state(&self) -> Option<&str> {
        if let Some(s) = &self.state {
            return Some(s);
        }
        self.agent?;
        Some(if self.last_output.elapsed() < WORKING_QUIET { "working" } else { "idle" })
    }
    pub fn label(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.parser.callbacks().title.clone())
            .or(self.agent.map(String::from))
            .unwrap_or_else(|| "shell".into())
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
    /// The main checkout's branch (shown as the project itself, like herdr).
    pub fn branch(&self) -> Option<&str> {
        self.worktrees.as_ref()?.iter().find(|w| w.main).map(|w| w.branch.as_str())
    }
    /// Worktrees listed under the project: all of them, except a main checkout on the default branch
    /// (that one *is* the project row).
    pub fn listed(&self) -> impl Iterator<Item = (usize, &git::Worktree)> {
        self.worktrees.iter().flatten().enumerate().filter(|(_, w)| !(w.main && is_default_branch(&w.branch)))
    }
    pub fn has_linked(&self) -> bool {
        self.listed().next().is_some()
    }
    /// Panes in the main checkout plus all linked worktrees.
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

pub enum PromptKind {
    AddProject(usize),
    NewWorkspace,
    NewWorktree(PathBuf),
}

pub struct Prompt {
    pub kind: PromptKind,
    pub label: &'static str,
    pub input: String,
}

#[derive(Clone, Copy)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

pub struct App {
    pub workspaces: Vec<Workspace>,
    pub views: HashMap<PathBuf, View>,
    pub panes: HashMap<u32, Pane>,
    next_id: u32,
    pub current: Option<PathBuf>,
    pub sidebar_focus: bool,
    pub show_sidebar: bool,
    pub sidebar_w: u16,
    drag_sidebar: bool,
    pub hover: Option<usize>,
    pub pointer: &'static str,
    pub dirty: bool,
    pub full_redraw: bool,
    mouse_pane: Option<u32>,
    pub sel: usize,
    pub prefix: bool,
    pub prompt: Option<Prompt>,
    pub w: u16,
    pub h: u16,
    pub folded: HashSet<String>,
    pub flash: Option<(String, Instant)>,
    socket: PathBuf,
    tx: Sender<Ev>,
    watcher: watch::Watcher,
    quit: bool,
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap()
}

pub fn ws_key(name: &str) -> String {
    format!("w:{name}")
}
pub fn proj_key(p: &Path) -> String {
    format!("p:{}", p.display())
}

impl App {
    fn new(tx: Sender<Ev>, socket: PathBuf) -> Self {
        let cfg = config::load().unwrap_or_else(|| {
            let cwd = std::env::current_dir().unwrap_or_default();
            let c = config::Config {
                workspace: vec![config::WsCfg { name: "default".into(), projects: vec![cwd.display().to_string()] }],
            };
            config::save(&c);
            c
        });
        let workspaces = cfg
            .workspace
            .into_iter()
            .map(|w| Workspace {
                name: w.name,
                projects: w.projects.iter().filter_map(|p| make_project(&config::expand(p))).collect(),
            })
            .collect();
        let (w, h) = terminal::size().unwrap_or((120, 40));
        let watcher = watch::Watcher::new(tx.clone());
        let app = App {
            workspaces,
            views: HashMap::new(),
            panes: HashMap::new(),
            next_id: 1,
            current: None,
            sidebar_focus: true,
            show_sidebar: true,
            sidebar_w: 30,
            drag_sidebar: false,
            hover: None,
            pointer: "text",
            dirty: true,
            full_redraw: false,
            mouse_pane: None,
            sel: 0,
            prefix: false,
            prompt: None,
            w,
            h,
            folded: HashSet::new(),
            flash: None,
            socket,
            tx,
            watcher,
            quit: false,
        };
        app.rewatch();
        app
    }

    fn save(&self) {
        let c = config::Config {
            workspace: self
                .workspaces
                .iter()
                .map(|w| config::WsCfg {
                    name: w.name.clone(),
                    projects: w.projects.iter().map(|p| config::tilde(&p.path)).collect(),
                })
                .collect(),
        };
        config::save(&c);
    }

    fn rewatch(&self) {
        let paths = self.workspaces.iter().flat_map(|w| &w.projects).flat_map(|p| git::watch_paths(&p.path)).collect();
        self.watcher.set(paths);
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
            Row::Proj(wi, pi) => {
                Some(self.workspaces[wi].projects[pi].path.clone())
            }
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

    pub fn pane_area(&self) -> Rect {
        let x = if self.show_sidebar { self.sidebar_w + 1 } else { 0 };
        Rect { x, y: 0, w: self.w.saturating_sub(x), h: self.h.saturating_sub(1) }
    }

    /// Content rects (header row excluded) of the current view's panes, side by side.
    pub fn layout(&self) -> Vec<(u32, Rect)> {
        let Some(cur) = &self.current else { return vec![] };
        let ids = self.panes_in(cur);
        let a = self.pane_area();
        let n = ids.len() as u16;
        if n == 0 || a.w < n * 4 || a.h < 3 {
            return vec![];
        }
        let colw = (a.w - (n - 1)) / n;
        let mut x = a.x;
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                let w = if i as u16 == n - 1 { a.x + a.w - x } else { colw };
                let r = Rect { x, y: a.y + 1, w, h: a.h - 1 };
                x += w + 1;
                (*id, r)
            })
            .collect()
    }

    fn apply_layout(&mut self) {
        for (id, r) in self.layout() {
            let p = self.panes.get_mut(&id).unwrap();
            if p.rows != r.h || p.cols != r.w {
                p.rows = r.h;
                p.cols = r.w;
                p.parser.set_size(nz(r.h), nz(r.w));
                pty::resize(&p.master, r.h, r.w);
            }
        }
    }

    pub fn focused(&self) -> Option<u32> {
        let v = self.views.get(self.current.as_ref()?)?;
        v.panes.get(v.focus).copied()
    }

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
        let n = self.panes_in(view).len() as u16 + 1;
        let (rows, cols) = (a.h.saturating_sub(1).max(2), (a.w / n).max(10));
        let pty = pty::spawn(view, rows, cols, &env).map_err(|e| e.to_string())?;
        let mut r = pty.master.try_clone().map_err(|e| e.to_string())?;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(Ev::Output(id, buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = tx.send(Ev::Exit(id));
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
                parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), 5000, Cb::default()),
                master: pty.master,
                shell: pgid,
                pgid,
                view: view.to_path_buf(),
                title: None,
                state: None,
                agent: None,
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

    fn handle(&mut self, ev: Ev) {
        // Plain mouse motion only matters if it changes hover/pointer: don't redraw otherwise.
        if let Ev::Input(Event::Mouse(m)) = &ev {
            if m.kind == MouseEventKind::Moved {
                return self.on_hover(m.column, m.row);
            }
        }
        self.dirty = true;
        match ev {
            Ev::Input(Event::Key(k)) if k.kind != KeyEventKind::Release => self.on_key(k),
            Ev::Input(Event::Paste(s)) => {
                if let Some(id) = self.focused().filter(|_| !self.sidebar_focus && self.prompt.is_none()) {
                    let bp = self.panes[&id].parser.screen().bracketed_paste();
                    let data = if bp { format!("\x1b[200~{s}\x1b[201~") } else { s };
                    self.write_pane(id, data.as_bytes());
                } else if let Some(p) = &mut self.prompt {
                    p.input.push_str(s.trim());
                }
            }
            Ev::Input(Event::Resize(w, h)) => (self.w, self.h) = (w, h),
            Ev::Input(Event::Mouse(m)) => self.on_mouse(m),
            Ev::Input(_) => {}
            Ev::Output(id, data) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.parser.process(&data);
                    let now = Instant::now();
                    if now - p.last_input > Duration::from_millis(300) {
                        p.last_output = now;
                    }
                    let reply = std::mem::take(&mut p.parser.callbacks_mut().reply);
                    if !reply.is_empty() {
                        let _ = p.master.write_all(&reply);
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
            }
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
            Ev::Api(req, rtx) => {
                let r = self.on_api(req);
                let _ = rtx.send(r);
            }
            Ev::Git => {
                self.refresh_git();
                self.rewatch();
            }
        }
    }

    /// First visible sidebar row index (rows are drawn from y=2).
    pub fn sidebar_top(&self, nrows: usize) -> usize {
        let vis = self.h.saturating_sub(3) as usize;
        let sel = self.sel.min(nrows.saturating_sub(1));
        (sel + 1).saturating_sub(vis)
    }

    fn on_hover(&mut self, x: u16, y: u16) {
        let (hover, pointer) = if !self.show_sidebar || self.prompt.is_some() || x > self.sidebar_w {
            (None, "text")
        } else if x == self.sidebar_w {
            (None, "col-resize")
        } else {
            let n = self.rows().len();
            let i = self.sidebar_top(n) + y.saturating_sub(2) as usize;
            if y >= 2 && i < n { (Some(i), "pointer") } else { (None, "default") }
        };
        if hover != self.hover || pointer != self.pointer {
            self.hover = hover;
            self.pointer = pointer;
            self.dirty = true;
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        if self.drag_sidebar {
            match m.kind {
                MouseEventKind::Drag(_) => self.sidebar_w = x.clamp(16, (self.w / 2).max(16)),
                _ => self.drag_sidebar = false,
            }
            return;
        }
        if let (MouseEventKind::Drag(_) | MouseEventKind::Up(_), Some(id)) = (m.kind, self.mouse_pane) {
            if matches!(m.kind, MouseEventKind::Up(_)) {
                self.mouse_pane = None;
            }
            // Drags/releases may leave the pane: clamp to its content area.
            let Some((_, r)) = self.layout().into_iter().find(|(i, _)| *i == id) else { return };
            let column = x.clamp(r.x, r.x + r.w - 1) - r.x;
            let row = y.clamp(r.y, r.y + r.h - 1) - r.y;
            return self.forward_mouse(id, MouseEvent { column, row, ..m });
        }
        let sb = self.show_sidebar && self.prompt.is_none();
        if sb && x == self.sidebar_w {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                self.drag_sidebar = true;
            }
            return;
        }
        if sb && x < self.sidebar_w {
            let rows = self.rows();
            match m.kind {
                MouseEventKind::ScrollUp => self.sel = self.sel.saturating_sub(1),
                MouseEventKind::ScrollDown => self.sel = (self.sel + 1).min(rows.len().saturating_sub(1)),
                MouseEventKind::Down(MouseButton::Left) if y >= 2 => {
                    let i = self.sidebar_top(rows.len()) + (y - 2) as usize;
                    let Some(&row) = rows.get(i) else { return };
                    self.sel = i;
                    if let Row::Proj(w, p) = row {
                        let proj = &self.workspaces[w].projects[p];
                        if proj.has_linked() && (3..5).contains(&x) {
                            let key = proj_key(&proj.path);
                            if !self.folded.remove(&key) {
                                self.folded.insert(key);
                            }
                            return;
                        }
                    }
                    match self.row_view(row) {
                        Some(v) => self.open_view(v),
                        None => {
                            let key = match row {
                                Row::Ws(w) => ws_key(&self.workspaces[w].name),
                                Row::Proj(w, p) => proj_key(&self.workspaces[w].projects[p].path),
                                Row::Wt(..) => return,
                            };
                            if !self.folded.remove(&key) {
                                self.folded.insert(key);
                            }
                            self.sidebar_focus = true;
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        // Pane area: header row counts as part of the pane.
        let hit = self.layout().into_iter().find(|(_, r)| x >= r.x && x < r.x + r.w && y + 1 >= r.y && y < r.y + r.h);
        let Some((id, r)) = hit else { return };
        if let MouseEventKind::Down(_) = m.kind {
            if let Some(v) = self.current.as_ref().and_then(|c| self.views.get_mut(c)) {
                v.focus = v.panes.iter().position(|p| *p == id).unwrap_or(0);
            }
            self.sidebar_focus = false;
        }
        if y < r.y {
            return; // header click: focus only
        }
        let local = MouseEvent { column: x - r.x, row: y - r.y, ..m };
        let s = self.panes[&id].parser.screen();
        if s.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
            if let MouseEventKind::Down(_) = m.kind {
                self.mouse_pane = Some(id);
            }
            self.forward_mouse(id, local);
        } else if s.alternate_screen() {
            // Full-screen app without mouse support (less, man): wheel → arrow keys.
            let key: &[u8] = match m.kind {
                MouseEventKind::ScrollUp => b"\x1b[A\x1b[A\x1b[A",
                MouseEventKind::ScrollDown => b"\x1b[B\x1b[B\x1b[B",
                _ => return,
            };
            self.write_pane(id, key);
        }
    }

    /// Encode a mouse event (pane-local coords) for the pane's app.
    fn forward_mouse(&mut self, id: u32, m: MouseEvent) {
        let Some(p) = self.panes.get(&id) else { return };
        let s = p.parser.screen();
        use vt100::MouseProtocolMode as Mode;
        let mode = s.mouse_protocol_mode();
        let btn = |b: MouseButton| match b { MouseButton::Left => 0, MouseButton::Middle => 1, MouseButton::Right => 2 };
        let (code, release) = match m.kind {
            MouseEventKind::Down(b) => (btn(b), false),
            MouseEventKind::Up(b) if mode != Mode::Press => (btn(b), true),
            MouseEventKind::Drag(b) if matches!(mode, Mode::ButtonMotion | Mode::AnyMotion) => (btn(b) + 32, false),
            MouseEventKind::ScrollUp => (64, false),
            MouseEventKind::ScrollDown => (65, false),
            _ => return,
        };
        let (cx, cy) = (m.column as u32 + 1, m.row as u32 + 1);
        let bytes = if s.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr {
            format!("\x1b[<{code};{cx};{cy}{}", if release { 'm' } else { 'M' }).into_bytes()
        } else {
            let c = if release { 3 } else { code };
            let mut v = b"\x1b[M".to_vec();
            v.extend([(32 + c) as u8, (32 + cx.min(223)) as u8, (32 + cy.min(223)) as u8]);
            v
        };
        let _ = self.panes.get_mut(&id).unwrap().master.write_all(&bytes);
    }

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl_a = k.code == KeyCode::Char('a') && k.modifiers.contains(KeyModifiers::CONTROL);
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        if self.prefix {
            self.prefix = false;
            return self.prefix_key(k, ctrl_a);
        }
        if ctrl_a {
            self.prefix = true;
            return;
        }
        if self.sidebar_focus || self.focused().is_none() {
            return self.sidebar_key(k);
        }
        let id = self.focused().unwrap();
        let ac = self.panes[&id].parser.screen().application_cursor();
        self.write_pane(id, &keys::encode(k, ac));
    }

    fn prefix_key(&mut self, k: KeyEvent, ctrl_a: bool) {
        let cur = self.current.clone();
        match k.code {
            _ if ctrl_a => {
                if let Some(id) = self.focused() {
                    self.write_pane(id, &[1]);
                }
            }
            KeyCode::Char('|' | '%' | '\\') => {
                if let Some(v) = cur {
                    if let Err(e) = self.spawn_pane(&v) {
                        self.say(e);
                    }
                    self.sidebar_focus = false;
                }
            }
            KeyCode::Char('h') | KeyCode::Left => self.move_focus(-1),
            KeyCode::Char('l') | KeyCode::Right => self.move_focus(1),
            KeyCode::Char('x') => {
                if let Some(id) = self.focused() {
                    self.kill_pane(id);
                }
            }
            KeyCode::Tab | KeyCode::Char('s') => self.sidebar_focus = !self.sidebar_focus || self.focused().is_none(),
            KeyCode::Char('b') => self.show_sidebar = !self.show_sidebar,
            KeyCode::Char('j') | KeyCode::Down => self.step_view(1),
            KeyCode::Char('k') | KeyCode::Up => self.step_view(-1),
            KeyCode::Char('n') => {
                let mut ids: Vec<_> = self.panes.values().filter(|p| p.state() == Some("waiting")).map(|p| (p.id, p.view.clone())).collect();
                ids.sort();
                if let Some((id, v)) = ids.into_iter().next() {
                    self.open_view(v.clone());
                    let view = self.views.get_mut(&v).unwrap();
                    view.focus = view.panes.iter().position(|x| *x == id).unwrap_or(0);
                }
            }
            KeyCode::Char('c') => {
                // Clear only this pane: wipe our copy of its screen, then let the app redraw (Ctrl-L).
                if let Some(id) = self.focused() {
                    let p = self.panes.get_mut(&id).unwrap();
                    p.parser.process(b"\x1b[H\x1b[2J\x1b[3J");
                    self.write_pane(id, &[0x0c]);
                }
            }
            KeyCode::Char('r') => self.full_redraw = true,
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn kill_pane(&mut self, id: u32) {
        if let Some(p) = self.panes.get(&id) {
            // Hang up the foreground job and the shell's own group.
            unsafe { libc::kill(-p.pgid, libc::SIGHUP) };
            unsafe { libc::kill(-p.shell, libc::SIGHUP) };
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

    fn sidebar_key(&mut self, k: KeyEvent) {
        let rows = self.rows();
        if rows.is_empty() {
            if k.code == KeyCode::Char('W') {
                self.prompt = Some(Prompt { kind: PromptKind::NewWorkspace, label: "new workspace", input: String::new() });
            } else if k.code == KeyCode::Char('q') {
                self.quit = true;
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
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.sel = (self.sel + 1).min(rows.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            // l/→ unfold first; Enter opens (or toggles a workspace).
            KeyCode::Char('l') | KeyCode::Right if fold_key.as_ref().is_some_and(|f| self.folded.contains(f)) => {
                self.folded.remove(fold_key.as_ref().unwrap());
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => match self.row_view(row) {
                Some(v) => self.open_view(v),
                None => {
                    if let Some(f) = fold_key {
                        if !self.folded.remove(&f) && k.code == KeyCode::Enter {
                            self.folded.insert(f);
                        }
                    }
                }
            },
            KeyCode::Char('h') | KeyCode::Left => match (row, fold_key) {
                (_, Some(f)) if !self.folded.contains(&f) => {
                    self.folded.insert(f);
                }
                (Row::Proj(w, _), Some(_)) => self.sel = rows.iter().position(|r| *r == Row::Ws(w)).unwrap_or(0),
                (Row::Wt(w, p, _), None) => self.sel = rows.iter().position(|r| *r == Row::Proj(w, p)).unwrap_or(0),
                (Row::Proj(w, _), None) => self.sel = rows.iter().position(|r| *r == Row::Ws(w)).unwrap_or(0),
                _ => {}
            },
            KeyCode::Char('a') => {
                self.prompt = Some(Prompt { kind: PromptKind::AddProject(wi), label: "add project (dir)", input: "~/".into() })
            }
            KeyCode::Char('W') => {
                self.prompt = Some(Prompt { kind: PromptKind::NewWorkspace, label: "new workspace", input: String::new() })
            }
            KeyCode::Char('w') => match row {
                Row::Proj(w, p) | Row::Wt(w, p, _) if self.workspaces[w].projects[p].worktrees.is_some() => {
                    let path = self.workspaces[w].projects[p].path.clone();
                    self.prompt = Some(Prompt { kind: PromptKind::NewWorktree(path), label: "new worktree branch", input: String::new() });
                }
                _ => self.say("not a git project".into()),
            },
            KeyCode::Char('x') => {
                if let Row::Proj(w, p) = row {
                    let proj = &self.workspaces[w].projects[p];
                    let busy = proj.view_paths().iter().any(|v| !self.panes_in(v).is_empty());
                    if busy {
                        self.say("close its panes first".into());
                    } else {
                        self.workspaces[w].projects.remove(p);
                        self.save();
                        self.rewatch();
                    }
                }
            }
            KeyCode::Char('D') => {
                if let Row::Wt(w, p, t) = row {
                    let proj = &self.workspaces[w].projects[p];
                    let wt = proj.worktrees.as_ref().unwrap()[t].clone();
                    if wt.main {
                        self.say("can't remove the main checkout".into());
                    } else if !self.panes_in(&wt.path).is_empty() {
                        self.say("close its panes first".into());
                    } else {
                        let r = git::remove(&proj.path, &wt.path);
                        self.say(match r { Ok(()) => format!("removed {}", wt.branch), Err(e) => e });
                        self.refresh_git();
                        self.rewatch();
                    }
                }
            }
            KeyCode::Esc | KeyCode::Tab => {
                if self.focused().is_some() {
                    self.sidebar_focus = false;
                }
            }
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn prompt_key(&mut self, k: KeyEvent) {
        let Some(mut p) = self.prompt.take() else { return };
        match k.code {
            KeyCode::Esc => {}
            KeyCode::Enter => self.submit(p),
            KeyCode::Backspace => {
                p.input.pop();
                self.prompt = Some(p);
            }
            KeyCode::Tab => {
                if matches!(p.kind, PromptKind::AddProject(_)) {
                    p.input = complete(&p.input);
                }
                self.prompt = Some(p);
            }
            KeyCode::Char(c) => {
                p.input.push(c);
                self.prompt = Some(p);
            }
            _ => self.prompt = Some(p),
        }
    }

    fn submit(&mut self, p: Prompt) {
        let input = p.input.trim().to_string();
        if input.is_empty() {
            return;
        }
        match p.kind {
            PromptKind::AddProject(wi) => match make_project(&config::expand(&input)) {
                Some(proj) if wi < self.workspaces.len() => {
                    self.say(format!("added {}", proj.name));
                    self.workspaces[wi].projects.push(proj);
                    self.save();
                    self.rewatch();
                }
                _ => self.say(format!("not a directory: {input}")),
            },
            PromptKind::NewWorkspace => {
                self.workspaces.push(Workspace { name: input, projects: vec![] });
                self.sel = self.rows().len() - 1;
                self.save();
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
                        s += &format!("  {}  {}{}\n", p.name, config::tilde(&p.path), String::new());
                        let mut views: Vec<(String, PathBuf)> = p.listed().map(|(_, t)| (t.branch.clone(), t.path.clone())).collect();
                        if !views.iter().any(|(_, v)| *v == p.path) {
                            views.insert(0, (String::new(), p.path.clone()));
                        }
                        for (branch, path) in views {
                            let ind = if branch.is_empty() { "    " } else {
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
                let text = self.panes[&id].parser.screen().contents();
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
            _ => Reply::err(format!("unknown command: {} (see pmux --help)", a.join(" "))),
        }
    }

    /// When the earliest "working" heuristic flips to idle (so we can sleep until then).
    fn next_deadline(&self) -> Option<Instant> {
        let mut d = self.panes.values().filter(|p| p.agent.is_some() && p.state.is_none() && p.last_output.elapsed() < WORKING_QUIET).map(|p| p.last_output + WORKING_QUIET).min();
        if let Some((_, t)) = &self.flash {
            let f = *t + Duration::from_secs(4);
            if f > Instant::now() {
                d = Some(d.map_or(f, |d| d.min(f)));
            }
        }
        d
    }
}

fn make_project(path: &Path) -> Option<Project> {
    let path = std::fs::canonicalize(path).ok().filter(|p| p.is_dir())?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    let worktrees = git::worktrees(&path);
    Some(Project { path, name, worktrees })
}

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

pub fn run() -> io::Result<()> {
    let (tx, rx) = channel();
    let socket = crate::api::listen(tx.clone())?;
    let mut out = io::BufWriter::with_capacity(1 << 17, io::stdout());
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, crossterm::event::EnableBracketedPaste, crossterm::event::EnableMouseCapture)?;
    let sock2 = socket.clone();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        restore();
        let _ = std::fs::remove_file(&sock2);
        hook(i);
    }));
    let itx = tx.clone();
    std::thread::spawn(move || {
        while let Ok(e) = crossterm::event::read() {
            if itx.send(Ev::Input(e)).is_err() {
                break;
            }
        }
    });
    let mut app = App::new(tx, socket.clone());
    if let Ok(cwd) = std::env::current_dir().and_then(std::fs::canonicalize) {
        if app.rows().iter().any(|r| app.row_view(*r).as_ref() == Some(&cwd)) {
            app.open_view(cwd);
        }
    }
    let mut renderer = render::Renderer::default();
    loop {
        if app.dirty {
            app.dirty = false;
            app.apply_layout();
            if std::mem::take(&mut app.full_redraw) {
                renderer.invalidate();
            }
            let (frame, cursor) = render::compose(&app);
            renderer.draw(&mut out, frame, cursor, app.pointer)?;
        }
        // Sleep until an event arrives, or until a time-based state change is due. No idle ticks.
        let ev = match app.next_deadline() {
            Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                Ok(e) => Some(e),
                Err(RecvTimeoutError::Timeout) => {
                    app.dirty = true;
                    None
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(e) => Some(e),
                Err(_) => break,
            },
        };
        if let Some(e) = ev {
            app.handle(e);
        }
        // Coalesce bursts (e.g. heavy output) into one frame.
        let start = Instant::now();
        while let Ok(e) = rx.try_recv() {
            app.handle(e);
            if start.elapsed() > Duration::from_millis(8) {
                break;
            }
        }
        if app.quit {
            break;
        }
    }
    let ids: Vec<u32> = app.panes.keys().copied().collect();
    for id in ids {
        app.kill_pane(id);
    }
    let _ = std::fs::remove_file(&socket);
    restore();
    std::process::exit(0);
}

fn restore() {
    let mut o = io::stdout();
    let _ = o.write_all(b"\x1b]22;text\x1b\\");
    let _ = execute!(o, crossterm::event::DisableMouseCapture, crossterm::event::DisableBracketedPaste, terminal::LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = terminal::disable_raw_mode();
}
