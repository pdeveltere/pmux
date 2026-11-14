//! Panes: terminals (shells, PTY I/O, agent detection, mouse reporting) and editors.
use super::{App, Ev, Tab, R};
use crate::editor::Editor;
use crate::platform;
use crate::pty;
use crate::term::Vt;
use alacritty_terminal::event::{Event as TEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use winit::event::MouseButton;
use winit::keyboard::ModifiersState;

/// An agent with no output for this long counts as idle.
const WORKING_QUIET: Duration = Duration::from_millis(1500);
/// Output this soon after typing is the echo, not the agent working.
const ECHO_GRACE: Duration = Duration::from_millis(300);
/// How often to check which process owns the terminal.
const PROBE_EVERY: Duration = Duration::from_millis(250);

/// A pane's state as the sidebar shows it. Order is urgency: the most urgent wins in a summary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum State {
    Idle,
    Done,
    Working,
    Error,
    Waiting,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Done => "done",
            State::Working => "working",
            State::Error => "error",
            State::Waiting => "waiting",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [State::Idle, State::Done, State::Working, State::Error, State::Waiting].into_iter().find(|st| st.as_str() == s)
    }
}

pub struct Pane {
    pub id: u32,
    pub vt: Vt,
    /// The PTY; `None` for an editor pane.
    master: Option<File>,
    /// Set for an editor pane (then `vt` is an idle placeholder).
    pub editor: Option<Editor>,
    shell: i32,
    /// Foreground process group, last seen.
    pgid: i32,
    pub view: std::path::PathBuf,
    /// Set with `pmux title`.
    pub title: Option<String>,
    /// Set by the program (OSC 0/2).
    pub osc_title: Option<String>,
    /// Set with `pmux status`; overrides the heuristic.
    pub state: Option<State>,
    pub agent: Option<&'static str>,
    pub bell: bool,
    last_output: Instant,
    last_input: Instant,
    last_probe: Instant,
    pub rows: usize,
    pub cols: usize,
}

impl Pane {
    /// Effective state: the one set explicitly, else a bell, else working/idle from output for agents.
    pub fn state(&self) -> Option<State> {
        if self.state.is_some() {
            return self.state;
        }
        if self.bell {
            return Some(State::Waiting);
        }
        self.agent?;
        Some(if self.last_output.elapsed() < WORKING_QUIET { State::Working } else { State::Idle })
    }

    /// When the working → idle heuristic flips next, if it will.
    pub fn state_change_at(&self) -> Option<Instant> {
        let at = self.last_output + WORKING_QUIET;
        (self.agent.is_some() && self.state.is_none() && at > Instant::now()).then_some(at)
    }

    pub fn label(&self) -> String {
        if let Some(e) = &self.editor {
            return e.name();
        }
        self.title.clone().or_else(|| self.osc_title.clone()).or(self.agent.map(String::from)).unwrap_or_else(|| "shell".into())
    }

    /// The agent's name, or "shell".
    pub fn kind(&self) -> &'static str {
        if self.editor.is_some() {
            return "editor";
        }
        self.agent.unwrap_or("shell")
    }

    /// Write to the PTY (keyboard input, query replies).
    pub(super) fn write(&mut self, bytes: &[u8]) {
        if let Some(m) = &mut self.master {
            let _ = m.write_all(bytes);
        }
    }

    /// Send Enter after a short delay, so agents don't take it as part of a paste.
    pub(super) fn enter_later(&self) {
        if let Some(Ok(mut f)) = self.master.as_ref().map(File::try_clone) {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                let _ = f.write_all(b"\r");
            });
        }
    }

    pub(super) fn resize(&mut self, cols: usize, rows: usize, (cw, ch): (u16, u16)) {
        if self.rows != rows || self.cols != cols {
            self.rows = rows;
            self.cols = cols;
            if let Some(m) = &self.master {
                self.vt.resize(cols, rows);
                pty::resize(m, rows as u16, cols as u16, cols as u16 * cw, rows as u16 * ch);
            }
        }
    }

    /// Hang up the shell and whatever runs in the foreground.
    fn hangup(&self) {
        if self.master.is_none() {
            return;
        }
        unsafe {
            libc::kill(-self.pgid, libc::SIGHUP);
            libc::kill(-self.shell, libc::SIGHUP);
        }
    }

    /// Which process owns the terminal: detects agents starting and exiting (throttled syscall).
    fn probe_agent(&mut self, now: Instant) {
        let Some(master) = &self.master else { return };
        if now - self.last_probe <= PROBE_EVERY {
            return;
        }
        self.last_probe = now;
        let pg = pty::fg_pgrp(master);
        if pg != self.pgid {
            self.pgid = pg;
            let a = pty::proc_path(pg).as_deref().and_then(pty::agent_of);
            if a != self.agent {
                self.agent = a;
                self.state = None;
            }
        }
    }
}

/// Read the PTY on a thread, forwarding output to the event loop until it closes.
fn pump(id: u32, mut r: File, tx: super::Tx) {
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
}

#[derive(Clone, Copy)]
pub(super) enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    WheelUp,
    WheelDown,
}

impl App {
    /// New pane in `view` with its shell in `cwd` (a folder inside the view): beside the others
    /// in the shown tab, or in a new tab.
    pub(super) fn spawn_pane(&mut self, view: &Path, cwd: &Path, new_tab: bool) -> Result<u32, String> {
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
        // Size it as it will be laid out, so the shell starts at the right width.
        let a = self.pane_area();
        let n = if new_tab { 1 } else { self.tab_panes(view).len() + 1 };
        let body = R { w: a.w / n as f32, ..a }.inset(self.px(super::PAD));
        let (cols, rows) = self.grid_size(body);
        let pty = pty::spawn(cwd, rows as u16, cols as u16, &env).map_err(|e| e.to_string())?;
        pump(id, pty.master.try_clone().map_err(|e| e.to_string())?, self.tx.clone());
        let mut child = pty.child;
        let pgid = child.id() as i32;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let pane = Pane::new(id, view, Vt::new(cols, rows), Some(pty.master), None, pgid, (cols, rows));
        self.insert_pane(pane, view, new_tab);
        Ok(id)
    }

    /// New editor pane on `path`, in a new tab of `view`. Returns its id.
    pub(super) fn spawn_editor(&mut self, view: &Path, path: &Path) -> Result<u32, String> {
        let ed = Editor::open(path)?;
        let id = self.next_id;
        self.next_id += 1;
        let pane = Pane::new(id, view, Vt::new(2, 1), None, Some(ed), 0, (0, 0));
        self.insert_pane(pane, view, true);
        self.apply_layout();
        Ok(id)
    }

    fn insert_pane(&mut self, pane: Pane, view: &Path, new_tab: bool) {
        let id = pane.id;
        self.panes.insert(id, pane);
        let v = self.views.entry(view.to_path_buf()).or_default();
        if new_tab || v.tabs.is_empty() {
            v.tabs.push(Tab::default());
            v.tab = v.tabs.len() - 1;
        }
        let t = v.cur_mut().expect("tab exists");
        t.panes.push(id);
        t.focus = t.panes.len() - 1;
        self.dirty = true;
    }
}

impl Pane {
    fn new(id: u32, view: &Path, vt: Vt, master: Option<File>, editor: Option<Editor>, pgid: i32, (cols, rows): (usize, usize)) -> Self {
        let long_ago = Instant::now() - Duration::from_secs(60);
        Pane {
            id,
            vt,
            master,
            editor,
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
        }
    }
}

impl App {
    /// Input to a pane (marks it as typed-into, for the working heuristic).
    pub(super) fn write_pane(&mut self, id: u32, bytes: &[u8]) {
        if let Some(p) = self.panes.get_mut(&id) {
            p.last_input = Instant::now();
            p.write(bytes);
        }
    }

    /// Typing into a pane: jump back to the live screen first.
    pub(super) fn type_pane(&mut self, id: u32, bytes: &[u8]) {
        if let Some(p) = self.panes.get_mut(&id) {
            if p.vt.display_offset() > 0 {
                p.vt.scroll(Scroll::Bottom);
            }
        }
        self.write_pane(id, bytes);
    }

    /// Ask a pane to close; it's removed when its PTY closes (`on_exit`).
    /// An editor closes at once; with unsaved changes only on the second try.
    pub(super) fn kill_pane(&mut self, id: u32) {
        let Some(p) = self.panes.get_mut(&id) else { return };
        if let Some(e) = &mut p.editor {
            if e.modified() && !e.close_armed {
                e.close_armed = true;
                let name = e.name();
                return self.say(format!("{name} has unsaved changes: close again to discard, ⌘S to save"));
            }
            return self.on_exit(id);
        }
        p.hangup();
    }

    pub(super) fn kill_panes(&mut self, ids: impl IntoIterator<Item = u32>) {
        for id in ids {
            self.kill_pane(id);
        }
    }

    pub fn kill_all(&mut self) {
        let ids: Vec<u32> = self.panes.keys().copied().collect();
        self.kill_panes(ids);
    }

    pub(super) fn on_exit(&mut self, id: u32) {
        if let Some(p) = self.panes.remove(&id) {
            if let Some(v) = self.views.get_mut(&p.view) {
                v.remove(id);
            }
            self.see_focused();
        }
        if self.focused().is_none() {
            self.focus = super::Focus::Sidebar;
        }
    }

    pub(super) fn on_output(&mut self, id: u32, data: &[u8]) {
        let focused = self.focused();
        let (cw, ch) = (self.cell_w as u16, self.cell_h as u16);
        let Some(p) = self.panes.get_mut(&id) else { return };
        let events = p.vt.feed(data);
        let now = Instant::now();
        if now - p.last_input > ECHO_GRACE {
            p.last_output = now;
        }
        for e in events {
            match e {
                TEvent::PtyWrite(s) => p.write(s.as_bytes()),
                TEvent::Title(t) => p.osc_title = Some(t),
                TEvent::ResetTitle => p.osc_title = None,
                TEvent::Bell => p.bell = Some(id) != focused,
                TEvent::ColorRequest(i, f) => {
                    let rgb = p.vt.term.colors()[i].unwrap_or_else(|| crate::ui::default_color(i));
                    p.write(f(rgb).as_bytes());
                }
                TEvent::TextAreaSizeRequest(f) => {
                    let ws = WindowSize { num_lines: p.rows as u16, num_cols: p.cols as u16, cell_width: cw, cell_height: ch };
                    p.write(f(ws).as_bytes());
                }
                TEvent::ClipboardStore(_, s) => platform::clipboard_set(&s),
                _ => {}
            }
        }
        p.probe_agent(now);
    }

    /// Encode a mouse event for the pane's app (xterm / SGR).
    pub(super) fn pane_mouse(&mut self, id: u32, kind: MouseKind, x: f32, y: f32) {
        let Some(pr) = self.layout().into_iter().find(|p| p.id == id) else { return };
        let (cw, ch, mods) = (self.cell_w, self.cell_h, self.mods);
        let Some(p) = self.panes.get_mut(&id) else { return };
        let mode = p.vt.mode();
        let cell = |v: f32, size: f32, n: usize| ((v / size).floor().max(0.0) as usize).min(n.saturating_sub(1)) as u32 + 1;
        let col = cell(x - pr.body.x, cw, p.cols);
        let row = cell(y - pr.body.y, ch, p.rows);
        let btn = |b: MouseButton| match b {
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
        let mut m = mods;
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
        p.write(&bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::State;

    #[test]
    fn state_names_and_urgency() {
        for s in [State::Idle, State::Done, State::Working, State::Error, State::Waiting] {
            assert_eq!(State::parse(s.as_str()), Some(s));
        }
        assert_eq!(State::parse("clear"), None);
        assert!(State::Waiting > State::Error && State::Error > State::Working && State::Working > State::Done && State::Done > State::Idle);
    }
}
