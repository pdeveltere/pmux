//! Project search tabs (⌘⇧F), like Zed's: a query bar over the matching lines of every file in
//! the worktree. Matching lives in `crate::search`.
use super::{App, Focus, R};
use crate::editor::Pos;
use crate::input;
use crate::search::{Found, ProjectSearch, Query, Row};
use crate::term::Vt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use winit::event::KeyEvent;
use winit::keyboard::{ModifiersState, NamedKey};

/// Where a search tab's parts go inside its pane body.
pub struct SearchGeom {
    pub bar: R,
    /// The query field.
    pub field: R,
    /// Match case, whole word, regex.
    pub toggles: [R; 3],
    /// The results list.
    pub list: R,
    pub row_h: f32,
}

impl SearchGeom {
    pub fn rows(&self) -> usize {
        ((self.list.h / self.row_h).floor() as usize).max(1)
    }
}

/// Which toggle of the bar.
#[derive(Clone, Copy)]
enum Toggle {
    Case,
    Word,
    Regex,
}

impl App {
    pub fn search_geom(&self, body: R) -> SearchGeom {
        let bar = R { h: self.px(super::edit::SEARCH_BAR_H).min(body.h), ..body };
        let field = R {
            x: bar.x + self.px(8.0),
            y: bar.y + self.px(6.0),
            w: self.px(360.0).min(bar.w * 0.6),
            h: bar.h - self.px(12.0),
        };
        let tw = self.px(28.0);
        let t = |k: f32| R { x: field.x + field.w + self.px(6.0) + k * (tw + self.px(2.0)), w: tw, ..field };
        let list = R { y: bar.y + bar.h + self.px(4.0), h: (body.h - bar.h - self.px(4.0)).max(0.0), ..body };
        let row_h = (self.cell_h + self.px(6.0)).round();
        SearchGeom { bar, field, toggles: [t(0.0), t(1.0), t(2.0)], list, row_h }
    }

    /// ⌘⇧F: the current worktree's search tab (opened if there's none), with the editor's
    /// selection as the query.
    pub(super) fn open_project_search(&mut self) {
        let Some(view) = self.current.clone() else { return self.say("open a worktree first") };
        let picked = self
            .focused()
            .filter(|_| self.focus == Focus::Panes)
            .and_then(|id| self.panes.get(&id)?.editor.as_ref()?.selected_text())
            .filter(|t| !t.is_empty() && !t.contains('\n'));
        let existing = self.panes_in(&view).into_iter().find(|id| self.panes[id].search.is_some());
        let id = match existing {
            Some(id) => id,
            None => {
                let id = self.next_id;
                self.next_id += 1;
                let mut pane = super::Pane::new(id, &view, Vt::new(2, 1), None, None, (0, 0));
                pane.search = Some(ProjectSearch::new(view.clone(), String::new()));
                self.insert_pane(pane, &view, true);
                self.apply_layout();
                id
            }
        };
        if let Some(v) = self.views.get_mut(&view) {
            v.reveal(id);
        }
        self.focus_panes();
        if let Some(t) = picked {
            if let Some(s) = self.search_mut(id) {
                s.query.text = t;
            }
            self.run_search(id);
        }
    }

    fn search_mut(&mut self, id: u32) -> Option<&mut ProjectSearch> {
        self.panes.get_mut(&id)?.search.as_mut()
    }

    /// The focused search tab, when the panes have the keyboard.
    pub(super) fn focused_search(&self) -> Option<u32> {
        self.focused().filter(|id| self.focus == Focus::Panes && self.panes.get(id).is_some_and(|p| p.search.is_some()))
    }

    /// Visible result rows of search pane `id`.
    fn search_rows(&self, id: u32) -> usize {
        self.layout().into_iter().find(|p| p.id == id).map_or(20, |pr| self.search_geom(pr.body).rows())
    }

    /// Search for the tab's query on worker threads, cancelling the search in flight.
    fn run_search(&mut self, id: u32) {
        let tx = self.tx.clone();
        let Some(s) = self.search_mut(id) else { return };
        if let Some((_, c)) = s.running.take() {
            c.store(true, Ordering::Relaxed);
        }
        s.error = None;
        let q = s.query.clone();
        if q.text.is_empty() {
            s.set_found(q, Found { files: vec![], truncated: false });
            s.searched = None;
            return;
        }
        let re = match q.build() {
            Ok(re) => re,
            Err(e) => {
                s.error = Some(e);
                return;
            }
        };
        static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let gen = GEN.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        s.running = Some((gen, cancel.clone()));
        let root = s.root.clone();
        std::thread::spawn(move || {
            if let Some(f) = crate::search::run(&root, &re, &cancel) {
                let _ = tx.send_event(super::Ev::Searched(id, gen, q, f));
            }
        });
    }

    /// A search finished (ignored when a newer one started or the tab closed).
    pub(super) fn search_done(&mut self, id: u32, gen: u64, q: Query, f: Found) {
        if let Some(s) = self.search_mut(id).filter(|s| s.running.as_ref().is_some_and(|r| r.0 == gen)) {
            s.set_found(q, f);
        }
    }

    /// A key without ⌘ for the focused search tab.
    pub(super) fn search_tab_key(&mut self, id: u32, k: &KeyEvent) {
        let (alt, ctrl, shift) = (self.mods.alt_key(), self.mods.control_key(), self.mods.shift_key());
        let vis = self.search_rows(id);
        let Some(s) = self.search_mut(id) else { return };
        let cn = ctrl.then(|| input::base_char(k, ModifiersState::empty())).flatten();
        match (input::named(k), cn) {
            (Some(NamedKey::Enter), _) => {
                // A changed query searches; otherwise Enter opens the selected match.
                if s.searched.as_ref() != Some(&s.query) {
                    return self.run_search(id);
                }
                let sel = s.sel;
                self.open_hit(id, sel);
            }
            (Some(NamedKey::ArrowDown), _) | (_, Some('n')) => s.step(1, vis),
            (Some(NamedKey::ArrowUp), _) | (_, Some('p')) => s.step(-1, vis),
            (Some(NamedKey::Tab), _) => s.step(if shift { -1 } else { 1 }, vis),
            (Some(NamedKey::PageDown), _) => s.step(vis as isize, vis),
            (Some(NamedKey::PageUp), _) => s.step(-(vis as isize), vis),
            (Some(NamedKey::Backspace), _) => {
                let q = &mut s.query.text;
                if alt {
                    let t = q.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_').len();
                    q.truncate(q[..t].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_').len());
                } else {
                    q.pop();
                }
            }
            _ if ctrl => {}
            _ => {
                if let Some(t) = &k.text {
                    s.query.text.extend(t.chars().filter(|c| !c.is_control()));
                }
            }
        }
    }

    /// ⌘ shortcuts of a focused search tab. Returns false for the app's own.
    pub(super) fn search_cmd_key(&mut self, k: &KeyEvent) -> bool {
        let Some(id) = self.focused_search() else { return false };
        let alt = self.mods.alt_key();
        match (input::named(k), input::base_char(k, ModifiersState::empty())) {
            // Zed's toggles: ⌥⌘C match case, ⌥⌘W whole word, ⌥⌘X regex.
            (_, Some('c')) if alt => self.search_toggle(id, Toggle::Case),
            (_, Some('w')) if alt => self.search_toggle(id, Toggle::Word),
            (_, Some('x')) if alt => self.search_toggle(id, Toggle::Regex),
            (Some(NamedKey::Backspace), _) => {
                if let Some(s) = self.search_mut(id) {
                    s.query.text.clear();
                }
            }
            (_, Some('c')) => {
                if let Some(s) = self.panes.get(&id).and_then(|p| p.search.as_ref()) {
                    crate::platform::clipboard_set(&s.query.text);
                }
            }
            _ => return false,
        }
        true
    }

    fn search_toggle(&mut self, id: u32, t: Toggle) {
        let Some(s) = self.search_mut(id) else { return };
        let f = match t {
            Toggle::Case => &mut s.query.case,
            Toggle::Word => &mut s.query.word,
            Toggle::Regex => &mut s.query.regex,
        };
        *f = !*f;
        // Like Zed: a toggle re-runs a search that's on screen.
        if s.searched.is_some() || s.error.is_some() {
            self.run_search(id);
        }
    }

    pub(super) fn search_paste(&mut self, id: u32, t: &str) {
        if let Some(s) = self.search_mut(id) {
            s.query.text.push_str(t.lines().next().unwrap_or(""));
        }
    }

    /// Open the match on result row `row` in an editor tab, with the match selected.
    fn open_hit(&mut self, id: u32, row: usize) {
        let Some(s) = self.panes.get(&id).and_then(|p| p.search.as_ref()) else { return };
        let Some((f, h)) = s.hit(row) else { return };
        let (path, line, (a, b)) = (s.root.join(&f.path), h.line, h.ranges[0]);
        self.open_file(&path);
        let Some(eid) = self.focused().filter(|e| *e != id) else { return };
        let rows = self
            .layout()
            .into_iter()
            .find(|p| p.id == eid)
            .and_then(|pr| Some(self.editor_geom(pr.body, self.panes.get(&eid)?.editor.as_ref()?).rows));
        let Some(ed) = self.panes.get_mut(&eid).and_then(|p| p.editor.as_mut()) else { return };
        ed.set_cursor(Pos::new(line, a), false);
        ed.set_cursor(Pos::new(line, b), true);
        // Put the match a third of the way down, as Zed does.
        let rows = rows.unwrap_or(30);
        ed.top = line.saturating_sub(rows / 3);
        self.reveal_cursor();
    }

    // ---- mouse ----

    pub(super) fn search_press(&mut self, id: u32, x: f32, y: f32) {
        let Some(pr) = self.layout().into_iter().find(|p| p.id == id) else { return };
        let g = self.search_geom(pr.body);
        let ts = [Toggle::Case, Toggle::Word, Toggle::Regex];
        if let Some(k) = g.toggles.iter().position(|t| t.contains(x, y)) {
            return self.search_toggle(id, ts[k]);
        }
        if !g.list.contains(x, y) {
            return;
        }
        let Some(s) = self.search_mut(id) else { return };
        let row = s.top + ((y - g.list.y) / g.row_h).floor() as usize;
        match s.rows.get(row) {
            Some(Row::Hit(..)) => {
                s.sel = row;
                self.open_hit(id, row);
            }
            // A file header opens the file at its first match.
            Some(Row::File(_)) => {
                s.sel = row + 1;
                self.open_hit(id, row + 1);
            }
            None => {}
        }
    }

    /// Wheel over a search tab: `n` lines, positive = up.
    pub(super) fn search_wheel(&mut self, id: u32, n: i32) {
        let vis = self.search_rows(id);
        if let Some(s) = self.search_mut(id) {
            s.scroll(-(n as isize) * 3, vis);
        }
    }
}
