//! Panes: terminals (shells, PTY I/O, agent detection, mouse reporting) and editors.
use super::{App, Tab, R};
use crate::editor::Editor;
use crate::hold::{Link, Spawn};
use crate::platform;
use crate::term::Vt;
use alacritty_terminal::event::{Event as TEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use std::path::Path;
use std::time::{Duration, Instant};
use winit::event::MouseButton;
use winit::keyboard::ModifiersState;

/// An agent with no output for this long counts as idle.
const WORKING_QUIET: Duration = Duration::from_millis(1500);
/// Output this soon after typing is the echo, not the agent working.
const ECHO_GRACE: Duration = Duration::from_millis(300);

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

/// A pane: a terminal (shell on a PTY) or, when `editor` is set, a file editor.
pub struct Pane {
    pub id: u32,
    pub vt: Vt,
    /// The terminal, held by the background pmux; `None` for an editor pane.
    link: Option<Link>,
    /// Set for an editor pane (then `vt` is an idle placeholder).
    pub editor: Option<Editor>,
    /// Set for a project search pane (⌘⇧F; then `vt` is an idle placeholder).
    pub search: Option<crate::search::ProjectSearch>,
    pub view: std::path::PathBuf,
    /// Set with `pmux title`.
    pub title: Option<String>,
    /// Set by the program (OSC 0/2).
    pub osc_title: Option<String>,
    /// Set with `pmux status`; overrides the heuristic.
    pub state: Option<State>,
    pub agent: Option<&'static str>,
    /// What the agent's screen says it's doing (from the background pmux, see `detect`).
    pub screen: Option<crate::detect::Screen>,
    /// When `screen` last changed.
    screen_at: Instant,
    pub bell: bool,
    /// The agent worked while out of sight: when it goes quiet it's done, until looked at.
    pub(super) unseen: bool,
    /// When the explicit `state` was set.
    pub(super) state_at: Instant,
    /// When the current burst of output began.
    work_start: Instant,
    last_output: Instant,
    last_input: Instant,
    pub rows: usize,
    pub cols: usize,
}

impl Pane {
    /// Effective state: the one set explicitly, else a bell, else what the agent's screen shows.
    /// Output only counts until the screen has given a verdict: an idle agent writes too (it
    /// redraws when resized).
    pub fn state(&self) -> Option<State> {
        use crate::detect::Screen;
        if self.state.is_some() {
            return self.state;
        }
        if self.bell || self.screen == Some(Screen::Blocked) {
            return Some(State::Waiting);
        }
        self.agent?;
        let working = match self.screen {
            Some(s) => s == Screen::Working,
            None => self.last_output.elapsed() < WORKING_QUIET,
        };
        Some(if working {
            State::Working
        } else if self.unseen {
            State::Done
        } else {
            State::Idle
        })
    }

    /// When the effective state began (for "how long" in the agents panel).
    pub fn since(&self) -> Instant {
        if self.state.is_some() {
            self.state_at
        } else if self.screen.is_some() {
            self.screen_at
        } else if self.bell || self.last_output.elapsed() >= WORKING_QUIET {
            self.last_output + WORKING_QUIET
        } else {
            self.work_start
        }
    }

    /// When the working → idle heuristic flips next, if it will.
    pub fn state_change_at(&self) -> Option<Instant> {
        let at = self.last_output + WORKING_QUIET;
        (self.agent.is_some() && self.state.is_none() && self.screen.is_none() && at > Instant::now()).then_some(at)
    }

    pub fn label(&self) -> String {
        if let Some(e) = &self.editor {
            return e.name();
        }
        if let Some(s) = &self.search {
            return s.title();
        }
        self.title
            .clone()
            .or_else(|| self.osc_title.clone())
            .or(self.agent.map(String::from))
            .unwrap_or_else(|| "shell".into())
    }

    /// The agent's name, or "shell".
    pub fn kind(&self) -> &'static str {
        if self.editor.is_some() {
            return "editor";
        }
        if self.search.is_some() {
            return "search";
        }
        self.agent.unwrap_or("shell")
    }

    /// Write to the terminal (keyboard input, query replies).
    pub(super) fn write(&mut self, bytes: &[u8]) {
        if let Some(l) = &self.link {
            l.input(self.id, bytes);
        }
    }

    /// Send Enter after a short delay, so agents don't take it as part of a paste.
    pub(super) fn enter_later(&self) {
        if let Some(l) = self.link.clone() {
            let id = self.id;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                l.input(id, b"\r");
            });
        }
    }

    /// A terminal pane (not an editor).
    pub fn is_term(&self) -> bool {
        self.link.is_some()
    }

    pub(super) fn resize(&mut self, cols: usize, rows: usize, (cw, ch): (u16, u16)) {
        if self.rows != rows || self.cols != cols {
            self.rows = rows;
            self.cols = cols;
            if let Some(l) = &self.link {
                self.vt.resize(cols, rows);
                l.resize(self.id, rows as u16, cols as u16, cols as u16 * cw, rows as u16 * ch);
            }
        }
    }

    /// The agent running in the terminal changed (seen by the background pmux).
    pub(super) fn set_agent(&mut self, name: Option<&str>) {
        let a = name.and_then(crate::pty::agent_of);
        if a != self.agent {
            self.agent = a;
            self.state = None;
            self.screen = None;
            self.unseen = false;
        }
    }

    pub(super) fn set_screen(&mut self, s: Option<crate::detect::Screen>) {
        if s != self.screen {
            self.screen = s;
            self.screen_at = Instant::now();
        }
    }
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
            // For agent hooks (`pmux integration install claude`): this pmux, wherever it's installed.
            ("PMUX_BIN", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "pmux".into())),
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
        let link = self.link()?;
        let env = env.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        link.spawn(
            id,
            &Spawn { cwd: cwd.to_path_buf(), view: view.to_path_buf(), rows: rows as u16, cols: cols as u16, env },
        );
        let pane = Pane::new(id, view, Vt::new(cols, rows), Some(link), None, (cols, rows));
        self.insert_pane(pane, view, new_tab);
        Ok(id)
    }

    /// New editor pane on `path`, in a new tab of `view`. Returns its id.
    pub(super) fn spawn_editor(&mut self, view: &Path, path: &Path) -> Result<u32, String> {
        let ed = Editor::open(path)?;
        let id = self.next_id;
        self.next_id += 1;
        let pane = Pane::new(id, view, Vt::new(2, 1), None, Some(ed), (0, 0));
        self.insert_pane(pane, view, true);
        self.apply_layout();
        self.highlight(id);
        self.editors_rewatch();
        Ok(id)
    }

    pub(super) fn insert_pane(&mut self, pane: Pane, view: &Path, new_tab: bool) {
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
    pub(super) fn new(
        id: u32,
        view: &Path,
        vt: Vt,
        link: Option<Link>,
        editor: Option<Editor>,
        (cols, rows): (usize, usize),
    ) -> Self {
        let long_ago = Instant::now() - Duration::from_secs(60);
        Pane {
            id,
            vt,
            link,
            editor,
            search: None,
            view: view.to_path_buf(),
            title: None,
            osc_title: None,
            state: None,
            agent: None,
            screen: None,
            screen_at: long_ago,
            bell: false,
            unseen: false,
            state_at: long_ago,
            work_start: long_ago,
            last_output: long_ago,
            last_input: long_ago,
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
            p.vt.term.selection = None;
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
        if p.search.is_some() {
            return self.on_exit(id);
        }
        if let Some(l) = &p.link {
            l.kill(id);
        }
    }

    pub(super) fn kill_panes(&mut self, ids: impl IntoIterator<Item = u32>) {
        for id in ids {
            self.kill_pane(id);
        }
    }

    pub(super) fn on_exit(&mut self, id: u32) {
        if let Some(p) = self.panes.remove(&id) {
            if let Some(v) = self.views.get_mut(&p.view) {
                v.remove(id);
            }
            if p.editor.is_some() {
                self.editors_rewatch();
            }
            if let Some((_, c)) = p.search.as_ref().and_then(|s| s.running.as_ref()) {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.see_focused();
        }
        if self.focused().is_none() {
            self.focus = super::Focus::Sidebar;
        }
    }

    /// Feed a pane's output to its terminal. Returns whether the window needs a redraw: the pane
    /// is in the shown view, or its state (the sidebar dot) changed.
    pub(super) fn on_output(&mut self, id: u32, data: &[u8]) -> bool {
        let focused = self.focused();
        let shown = self.shown(id);
        let (cw, ch) = (self.cell_w as u16, self.cell_h as u16);
        let current = self.current.as_deref();
        let Some(p) = self.panes.get_mut(&id) else { return false };
        // Its view's tab bar shows its label, even when it's in another tab.
        let in_view = current == Some(p.view.as_path());
        let before = p.state();
        let events = p.vt.feed(data);
        let now = Instant::now();
        if now - p.last_input > ECHO_GRACE {
            if now - p.last_output >= WORKING_QUIET {
                p.work_start = now;
            }
            p.last_output = now;
            p.unseen = !shown;
        }
        for e in events {
            match e {
                // The background pmux answers terminal queries.
                TEvent::Title(t) => p.osc_title = Some(t),
                TEvent::ResetTitle => p.osc_title = None,
                TEvent::Bell => p.bell = Some(id) != focused,
                TEvent::ColorRequest(i, f) => {
                    let rgb = p.vt.term.colors()[i].unwrap_or_else(|| crate::ui::default_color(i));
                    p.write(f(rgb).as_bytes());
                }
                TEvent::TextAreaSizeRequest(f) => {
                    let ws = WindowSize {
                        num_lines: p.rows as u16,
                        num_cols: p.cols as u16,
                        cell_width: cw,
                        cell_height: ch,
                    };
                    p.write(f(ws).as_bytes());
                }
                TEvent::ClipboardStore(_, s) => platform::clipboard_set(&s),
                _ => {}
            }
        }
        in_view || p.state() != before
    }

    /// Encode a mouse event for the pane's app (xterm / SGR).
    pub(super) fn pane_mouse(&mut self, id: u32, kind: MouseKind, x: f32, y: f32) {
        let Some(pr) = self.layout().into_iter().find(|p| p.id == id) else { return };
        let (cw, ch, mods) = (self.cell_w, self.cell_h, self.mods);
        let Some(p) = self.panes.get_mut(&id) else { return };
        let mode = p.vt.mode();
        let cell =
            |v: f32, size: f32, n: usize| ((v / size).floor().max(0.0) as usize).min(n.saturating_sub(1)) as u32 + 1;
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
    use super::{Pane, State, Vt};
    use crate::detect::Screen;
    use std::path::Path;
    use std::time::Instant;

    #[test]
    fn output_counts_as_working_only_without_a_screen_verdict() {
        let mut p = Pane::new(1, Path::new("/"), Vt::new(2, 1), None, None, (2, 1));
        p.agent = Some("claude");
        p.last_output = Instant::now();
        assert_eq!(p.state(), Some(State::Working));
        assert!(p.state_change_at().is_some());

        // An idle agent redrawing after a resize stays idle.
        p.set_screen(Some(Screen::Idle));
        assert_eq!(p.state(), Some(State::Idle));
        assert_eq!(p.state_change_at(), None);

        p.set_screen(Some(Screen::Working));
        assert_eq!(p.state(), Some(State::Working));
    }

    #[test]
    fn state_names_and_urgency() {
        for s in [State::Idle, State::Done, State::Working, State::Error, State::Waiting] {
            assert_eq!(State::parse(s.as_str()), Some(s));
        }
        assert_eq!(State::parse("clear"), None);
        assert!(
            State::Waiting > State::Error
                && State::Error > State::Working
                && State::Working > State::Done
                && State::Done > State::Idle
        );
    }
}
