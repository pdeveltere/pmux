//! Editor panes: opening files, keys, mouse and wheel for the focused editor.
use super::{App, Focus, R};
use crate::editor::{Editor, Move, Pos};
use crate::{input, platform};
use std::path::Path;
use std::time::{Duration, Instant};
use winit::event::KeyEvent;
use winit::keyboard::{ModifiersState, NamedKey};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Height of the find bar, in logical pixels.
pub const SEARCH_BAR_H: f32 = 38.0;

/// Where an editor's parts go inside its pane body.
pub struct EditorGeom {
    /// The find bar across the top, when open.
    pub bar: Option<R>,
    /// Line numbers, right-aligned.
    pub gutter: R,
    /// The text grid.
    pub text: R,
    pub rows: usize,
    pub cols: usize,
}

impl App {
    pub fn editor_geom(&self, body: R, ed: &Editor) -> EditorGeom {
        let bar = ed.search.as_ref().map(|_| R { h: self.px(SEARCH_BAR_H).min(body.h), ..body });
        let body = match bar {
            Some(b) => R { y: body.y + b.h, h: (body.h - b.h).max(0.0), ..body },
            None => body,
        };
        let digits = ed.lines.len().max(100).to_string().len() as f32;
        let gw = ((digits + 3.0) * self.cell_w).round();
        let gutter = R { w: gw.min(body.w), ..body };
        let text = R { x: body.x + gutter.w, w: (body.w - gutter.w).max(0.0), ..body };
        let rows = ((text.h / self.cell_h).floor() as usize).max(1);
        let cols = ((text.w / self.cell_w).floor() as usize).max(1);
        EditorGeom { bar, gutter, text, rows, cols }
    }

    /// Open `p` in an editor tab of the current view, or show the tab that already has it.
    pub(super) fn open_file(&mut self, p: &Path) {
        let Some(view) = self.current.clone() else { return };
        match self.open_file_in(&view, p) {
            Ok(_) => self.focus_panes(),
            Err(e) => self.say(format!("can't open {}: {e}", crate::util::file_name(p))),
        }
    }

    /// Show `p` in an editor tab of `view`, reusing the tab that already has it.
    pub(super) fn open_file_in(&mut self, view: &Path, p: &Path) -> Result<u32, String> {
        let existing = self.panes_in(view).into_iter().find(|id| self.panes[id].editor.as_ref().is_some_and(|e| e.path == p));
        let id = match existing {
            Some(id) => id,
            None => self.spawn_editor(view, p)?,
        };
        if let Some(v) = self.views.get_mut(view) {
            v.reveal(id);
        }
        self.recent_files.retain(|r| r != p);
        self.recent_files.insert(0, p.to_path_buf());
        self.recent_files.truncate(50);
        Ok(id)
    }

    /// Watch the files open in editors (after one opens or closes).
    pub(super) fn editors_rewatch(&self) {
        let mut paths: Vec<std::path::PathBuf> = self.panes.values().filter_map(|p| Some(p.editor.as_ref()?.path.clone())).collect();
        paths.sort();
        paths.dedup();
        self.disk_watcher.set(paths);
    }

    /// Files open in editors changed on disk: reload the buffers without unsaved edits; warn
    /// about the others (saving them asks before overwriting).
    pub(super) fn on_disk_change(&mut self, changed: &[std::path::PathBuf]) {
        let mut reloaded = Vec::new();
        let mut conflicts = Vec::new();
        for p in self.panes.values_mut() {
            let Some(e) = p.editor.as_mut().filter(|e| changed.contains(&e.path) && e.changed_on_disk()) else { continue };
            if e.modified() {
                conflicts.push(e.name());
            } else if e.reload().is_ok() {
                reloaded.push(p.id);
            }
        }
        for id in reloaded {
            self.highlight(id);
        }
        if !conflicts.is_empty() {
            self.say(format!("{} changed on disk; this tab has unsaved edits", conflicts.join(", ")));
        }
    }

    /// The focused pane's editor, when the panes have the keyboard.
    fn focused_editor(&mut self) -> Option<&mut Editor> {
        let id = self.focused().filter(|_| self.focus == Focus::Panes)?;
        self.panes.get_mut(&id)?.editor.as_mut()
    }

    pub(super) fn has_focused_editor(&mut self) -> bool {
        self.focused_editor().is_some()
    }

    /// Scroll the focused editor so its cursor shows.
    pub(super) fn reveal_cursor(&mut self) {
        let Some(id) = self.focused() else { return };
        let Some(pr) = self.layout().into_iter().find(|p| p.id == id) else { return };
        let Some(ed) = self.panes.get(&id).and_then(|p| p.editor.as_ref()) else { return };
        let g = self.editor_geom(pr.body, ed);
        if let Some(ed) = self.panes.get_mut(&id).and_then(|p| p.editor.as_mut()) {
            ed.refresh_search();
            ed.reveal(g.rows, g.cols);
        }
        self.highlight(id);
    }

    /// Re-highlight editor pane `id` on a thread if its text changed (one job at a time per
    /// buffer; the result arrives as `Ev::Highlighted`).
    pub(super) fn highlight(&mut self, id: u32) {
        let Some(ed) = self.panes.get_mut(&id).and_then(|p| p.editor.as_mut()) else { return };
        let Some((version, lang, src)) = ed.highlight_job() else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let hl = crate::syntax::highlight_src(lang, &src);
            let _ = tx.send_event(super::Ev::Highlighted(id, version, hl));
        });
    }

    fn page_rows(&self) -> usize {
        let Some(pr) = self.focused().and_then(|id| self.layout().into_iter().find(|p| p.id == id)) else { return 20 };
        ((pr.body.h / self.cell_h) as usize).saturating_sub(2).max(1)
    }

    pub(super) fn editor_paste(&mut self, s: &str) {
        if let Some(e) = self.focused_editor() {
            match &e.search {
                Some(f) if f.focused => {
                    let q = format!("{}{}", f.query, s.lines().next().unwrap_or(""));
                    e.set_query(q);
                }
                _ => e.insert(s),
            }
        }
        self.reveal_cursor();
    }

    /// A key without ⌘ for the focused editor.
    pub(super) fn editor_key(&mut self, k: &KeyEvent) {
        let m = self.mods;
        let (shift, alt) = (m.shift_key(), m.alt_key());
        let page = self.page_rows();
        let Some(e) = self.focused_editor() else { return };
        if e.search.as_ref().is_some_and(|s| s.focused) {
            return self.search_key(k);
        }
        let word = |a: Move, b: Move| if alt { a } else { b };
        match input::named(k) {
            Some(NamedKey::ArrowLeft) => e.move_to(word(Move::WordLeft, Move::Left), shift),
            Some(NamedKey::ArrowRight) => e.move_to(word(Move::WordRight, Move::Right), shift),
            Some(NamedKey::ArrowUp) => e.move_to(Move::Up(1), shift),
            Some(NamedKey::ArrowDown) => e.move_to(Move::Down(1), shift),
            Some(NamedKey::Home) => e.move_to(Move::Home, shift),
            Some(NamedKey::End) => e.move_to(Move::End, shift),
            Some(NamedKey::PageUp) => e.move_to(Move::Up(page), shift),
            Some(NamedKey::PageDown) => e.move_to(Move::Down(page), shift),
            Some(NamedKey::Backspace) => e.delete(word(Move::WordLeft, Move::Left)),
            Some(NamedKey::Delete) => e.delete(word(Move::WordRight, Move::Right)),
            Some(NamedKey::Enter) => e.newline(),
            Some(NamedKey::Tab) if shift => e.indent(-1),
            Some(NamedKey::Tab) => e.tab(),
            Some(NamedKey::Escape) if e.search.is_some() => e.close_search(),
            Some(NamedKey::Escape) => e.clear_selection(),
            _ if m.control_key() => return,
            _ => match &k.text {
                Some(t) if !t.chars().any(char::is_control) => e.type_str(t),
                _ => return,
            },
        }
        self.reveal_cursor();
    }

    /// A key without ⌘ while the find bar has the keyboard.
    fn search_key(&mut self, k: &KeyEvent) {
        let (shift, alt, ctrl) = (self.mods.shift_key(), self.mods.alt_key(), self.mods.control_key());
        let Some(e) = self.focused_editor() else { return };
        let Some(q) = e.search.as_ref().map(|s| s.query.clone()) else { return };
        match input::named(k) {
            // Esc closes the bar, leaving the match selected.
            Some(NamedKey::Escape) => e.close_search(),
            Some(NamedKey::Enter) => e.find_step(!shift),
            Some(NamedKey::Backspace) => {
                let mut q = q;
                if alt {
                    let t = q.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_').len();
                    q.truncate(q[..t].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_').len());
                } else {
                    q.pop();
                }
                e.set_query(q);
            }
            _ if ctrl => return,
            _ => match &k.text {
                Some(t) if !t.chars().any(char::is_control) => e.set_query(q + t),
                _ => return,
            },
        }
        self.reveal_cursor();
    }

    /// ⌘ shortcuts the editor handles. Returns false for the app's own.
    pub(super) fn editor_cmd_key(&mut self, k: &KeyEvent, ch: Option<char>) -> bool {
        let (shift, alt) = (self.mods.shift_key(), self.mods.alt_key());
        let Some(e) = self.focused_editor() else { return false };
        // While the find bar has the keyboard, text shortcuts don't reach the buffer.
        let finding = e.search.as_ref().is_some_and(|s| s.focused);
        let mut flash = None;
        let mut saved_settings = false;
        match (input::named(k), input::base_char(k, ModifiersState::empty()), ch.map(|c| c.to_ascii_lowercase())) {
            // Find bar: ⌘F open/focus, ⌘G / ⌘⇧G next/previous, ⌥⌘C match case.
            (_, Some('f'), _) if !shift => e.open_search(),
            (_, Some('g'), _) if e.search.is_some() => e.find_step(!shift),
            (_, Some('g'), _) => e.open_search(),
            (_, Some('c'), _) if alt && e.search.is_some() => e.toggle_case(),
            (_, _, Some('s')) => {
                let name = e.name();
                flash = Some(match e.save() {
                    Ok(()) if e.path == crate::settings::path() => {
                        saved_settings = true;
                        "settings applied".into()
                    }
                    Ok(()) => format!("saved {name}"),
                    Err(err) => format!("couldn't save {name}: {err}"),
                });
            }
            (Some(NamedKey::Backspace), _, _) if finding => e.set_query(String::new()),
            (_, _, Some('c')) if finding => platform::clipboard_set(&e.search.as_ref().map(|s| s.query.clone()).unwrap_or_default()),
            (Some(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Delete), _, _)
            | (_, _, Some('z' | 'y' | 'a' | 'x' | ']' | '['))
                if finding => {}
            (Some(NamedKey::ArrowLeft), _, _) => e.move_to(Move::Home, shift),
            (Some(NamedKey::ArrowRight), _, _) => e.move_to(Move::End, shift),
            (Some(NamedKey::ArrowUp), _, _) => e.move_to(Move::DocStart, shift),
            (Some(NamedKey::ArrowDown), _, _) => e.move_to(Move::DocEnd, shift),
            (Some(NamedKey::Backspace), _, _) => e.delete(Move::Home),
            (Some(NamedKey::Delete), _, _) => e.delete(Move::End),
            (_, _, Some('z')) if shift => e.redo(),
            (_, _, Some('z')) => e.undo(),
            (_, _, Some('y')) => e.redo(),
            (_, _, Some('a')) if !shift => e.select_all(),
            (_, _, Some('c')) => platform::clipboard_set(&e.copy()),
            (_, _, Some('x')) => platform::clipboard_set(&e.cut()),
            (_, _, Some(']')) => e.indent(1),
            (_, _, Some('[')) => e.indent(-1),
            _ => return false,
        }
        if let Some(f) = flash {
            // A settings file that doesn't parse says why instead.
            if !saved_settings || self.reload_settings() {
                self.say(f);
            }
        }
        self.reveal_cursor();
        true
    }

    /// Text position under the mouse in editor pane `id`.
    fn editor_pos_at(&self, id: u32, x: f32, y: f32) -> Option<Pos> {
        let pr = self.layout().into_iter().find(|p| p.id == id)?;
        let ed = self.panes.get(&id)?.editor.as_ref()?;
        let g = self.editor_geom(pr.body, ed);
        let row = ((y - g.text.y) / self.cell_h).floor();
        let line = (ed.top as f32 + row).max(0.0) as usize;
        if line >= ed.lines.len() {
            return Some(Pos::new(ed.lines.len() - 1, usize::MAX));
        }
        let cx = ((x - g.text.x) / self.cell_w + 0.5).floor().max(0.0) as usize + ed.left;
        Some(Pos::new(line, ed.col_at(line, cx)))
    }

    /// Left press in an editor: place the cursor, shift extends, double-click selects a word.
    pub(super) fn editor_press(&mut self, id: u32, x: f32, y: f32) {
        // A click on the find bar gives it the keyboard; one on the text takes it back.
        let on_bar = self.layout().into_iter().find(|p| p.id == id).and_then(|pr| {
            let ed = self.panes.get(&id)?.editor.as_ref()?;
            Some(self.editor_geom(pr.body, ed).bar?.contains(x, y))
        });
        if let Some(s) = self.panes.get_mut(&id).and_then(|p| p.editor.as_mut()).and_then(|e| e.search.as_mut()) {
            s.focused = on_bar == Some(true);
        }
        if on_bar == Some(true) {
            return;
        }
        let Some(p) = self.editor_pos_at(id, x, y) else { return };
        let shift = self.mods.shift_key();
        let now = Instant::now();
        let double = self.edit_click.is_some_and(|(q, t)| q == p && now - t < DOUBLE_CLICK);
        self.edit_click = Some((p, now));
        let Some(e) = self.panes.get_mut(&id).and_then(|p| p.editor.as_mut()) else { return };
        if double {
            e.select_word(p);
        } else {
            e.set_cursor(p, shift);
            self.edit_drag = Some(id);
        }
    }

    pub(super) fn editor_drag(&mut self, id: u32, x: f32, y: f32) {
        let Some(p) = self.editor_pos_at(id, x, y) else { return };
        if let Some(e) = self.panes.get_mut(&id).and_then(|p| p.editor.as_mut()) {
            e.set_cursor(p, true);
        }
        self.dirty = true;
        self.reveal_cursor();
    }

    /// Wheel over an editor: `n` lines, positive = up.
    pub(super) fn editor_wheel(&mut self, id: u32, n: i32) {
        let Some(e) = self.panes.get_mut(&id).and_then(|p| p.editor.as_mut()) else { return };
        e.scroll(-(n as isize) * 3);
    }
}
