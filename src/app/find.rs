//! "Go to file" popup (⌘P): geometry, keys and mouse. Matching lives in `crate::finder`.
use super::{App, R};
use crate::finder::Finder;
use crate::input;
use winit::event::{KeyEvent, MouseButton};
use winit::keyboard::{ModifiersState, NamedKey};

/// Popup width and its distance from the window top, logical px.
const FINDER_W: f32 = 560.0;
const FINDER_TOP: f32 = 44.0;
const INPUT_H: f32 = 40.0;
const ROW_H: f32 = 28.0;
const PAD: f32 = 4.0;
/// Most result rows shown at once.
const MAX_ROWS: usize = 12;

impl App {
    /// ⌘P: open the popup on the current view's files; again while open moves down the list.
    pub(super) fn open_finder(&mut self) {
        let Some(root) = self.current.clone() else { return self.say("open a worktree first") };
        self.menu = None;
        self.prompt = None;
        self.finder = Some(Finder::loading(root.clone()));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let files = crate::finder::list(&root);
            let _ = tx.send_event(super::Ev::FinderFiles(root, files));
        });
    }

    /// The file list for the ⌘P popup is ready (ignored if the popup closed or moved on).
    pub(super) fn finder_files(&mut self, root: std::path::PathBuf, files: Vec<String>) {
        if let Some(f) = self.finder.as_mut().filter(|f| f.root == root && f.loading) {
            f.set_files(files, &self.recent_files);
        }
    }

    pub(super) fn finder_key(&mut self, k: &KeyEvent, ch: Option<char>) {
        let m = self.mods;
        let vis = self.finder_rows();
        let Some(f) = self.finder.as_mut() else { return };
        let ctrl = m.control_key().then(|| input::base_char(k, ModifiersState::empty())).flatten();
        match (input::named(k), ctrl) {
            (Some(NamedKey::Escape), _) => self.finder = None,
            (Some(NamedKey::Enter), _) => {
                let i = f.sel;
                self.finder_open(i);
            }
            (Some(NamedKey::ArrowDown), _) | (_, Some('n')) => f.step(1, vis),
            (Some(NamedKey::ArrowUp), _) | (_, Some('p')) => f.step(-1, vis),
            (Some(NamedKey::PageDown), _) => f.step(vis as isize, vis),
            (Some(NamedKey::PageUp), _) => f.step(-(vis as isize), vis),
            (Some(NamedKey::Backspace), _) => {
                if m.super_key() || m.control_key() {
                    f.query.clear();
                } else if m.alt_key() {
                    let t = f.query.trim_end_matches(|c: char| !c.is_alphanumeric()).len();
                    f.query.truncate(f.query[..t].trim_end_matches(char::is_alphanumeric).len());
                } else {
                    f.query.pop();
                }
                f.update();
            }
            _ if m.super_key() => match ch.map(|c| c.to_ascii_lowercase()) {
                Some('p') => f.step(1, vis),
                Some('v') => {
                    if let Some(s) = crate::platform::clipboard_get() {
                        self.on_paste(&s);
                    }
                }
                Some('q') => self.quit = true,
                _ => {}
            },
            _ if m.control_key() => {}
            _ => {
                if let Some(t) = &k.text {
                    let before = f.query.len();
                    f.query.extend(t.chars().filter(|c| !c.is_control()));
                    if f.query.len() != before {
                        f.update();
                    }
                }
            }
        }
    }

    /// Open result `i` in an editor tab and close the popup.
    fn finder_open(&mut self, i: usize) {
        let Some(p) = self.finder.take().and_then(|f| f.path(i)) else { return };
        self.open_file(&p);
    }

    // ---- geometry ----

    /// Result rows that fit.
    pub fn finder_rows(&self) -> usize {
        let room = (self.h - self.px(FINDER_TOP) - self.px(INPUT_H) - self.px(40.0)) / self.px(ROW_H);
        (room.max(1.0) as usize).min(MAX_ROWS)
    }

    /// The whole popup.
    pub fn finder_rect(&self) -> R {
        let Some(f) = &self.finder else { return R::default() };
        let w = self.px(FINDER_W).min(self.w - self.px(32.0));
        let rows = f.matches.len().clamp(1, self.finder_rows());
        let h = self.px(INPUT_H) + self.hairline() + 2.0 * self.px(PAD) + rows as f32 * self.px(ROW_H);
        R { x: ((self.w - w) / 2.0).round(), y: self.px(FINDER_TOP), w, h }
    }

    pub fn finder_input(&self) -> R {
        R { h: self.px(INPUT_H), ..self.finder_rect() }
    }

    /// Rect of visible row `k` (0 = first visible).
    pub fn finder_row(&self, k: usize) -> R {
        let r = self.finder_rect();
        let pad = self.px(PAD);
        R { x: r.x + pad, y: r.y + self.px(INPUT_H) + self.hairline() + pad + k as f32 * self.px(ROW_H), w: r.w - 2.0 * pad, h: self.px(ROW_H) }
    }

    /// Result index under (x, y).
    fn finder_hit(&self, x: f32, y: f32) -> Option<usize> {
        let f = self.finder.as_ref()?;
        let n = f.matches.len().saturating_sub(f.top).min(self.finder_rows());
        (0..n).find(|&k| self.finder_row(k).contains(x, y)).map(|k| f.top + k)
    }

    // ---- mouse ----

    pub(super) fn finder_hover(&mut self, x: f32, y: f32) -> bool {
        let h = self.finder_hit(x, y);
        let Some(f) = self.finder.as_mut() else { return false };
        if f.hover != h {
            f.hover = h;
            self.dirty = true;
        }
        h.is_some()
    }

    /// A click opens the result under it; outside the popup it dismisses it.
    pub(super) fn finder_click(&mut self, b: MouseButton, x: f32, y: f32) {
        match self.finder_hit(x, y) {
            Some(i) if b == MouseButton::Left => self.finder_open(i),
            _ if !self.finder_rect().contains(x, y) => self.finder = None,
            _ => {}
        }
    }

    pub(super) fn finder_wheel(&mut self, n: i32) {
        let vis = self.finder_rows();
        if let Some(f) = self.finder.as_mut() {
            f.scroll(n as isize, vis);
        }
    }
}
