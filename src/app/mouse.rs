//! Mouse: hover state and cursor shape, clicks, panel resizing, row drags, the wheel.
use super::pane::MouseKind;
use super::{proj_key, ws_key, App, Drag, Edge, Row, TabHit};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use std::time::{Duration, Instant};
use winit::event::{ElementState, MouseButton};
use winit::window::CursorIcon;

/// How far a pressed row moves before it becomes a drag.
const DRAG_START: f32 = 4.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

impl App {
    fn set_cursor(&mut self, icon: CursorIcon) {
        if icon != self.cursor_icon {
            self.cursor_icon = icon;
            self.dirty = true;
        }
    }

    pub fn on_cursor_moved(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        if self.menu.is_some() {
            let h = self.menu_item_at(x, y);
            let m = self.menu.as_mut().expect("menu is open");
            if m.hover != h {
                m.hover = h;
                self.dirty = true;
            }
            return self.set_cursor(if h.is_some() { CursorIcon::Pointer } else { CursorIcon::Default });
        }
        if self.finder.is_some() {
            let on = self.finder_hover(x, y);
            return self.set_cursor(if on { CursorIcon::Pointer } else { CursorIcon::Default });
        }
        if let Some(edge) = self.resizing {
            match edge {
                Edge::Sidebar => self.sidebar_w = self.panel_width(x),
                Edge::Files => self.files_w = self.panel_width(self.w - x),
                Edge::Agents => self.resize_agents(y),
            }
            self.dirty = true;
            return;
        }
        if let Some(id) = self.edit_drag {
            return self.editor_drag(id, x, y);
        }
        if let Some(id) = self.term_drag {
            return self.term_select_drag(id, x, y);
        }
        if let Some((id, b)) = self.mouse_pane {
            return self.pane_mouse(id, MouseKind::Drag(b), x, y);
        }
        if let Some(d) = &self.drag {
            if d.active || (y - d.start_y).abs() > self.px(DRAG_START) {
                let target = if self.sidebar_rect().contains(x, y) { self.drop_target(d.row, y) } else { None };
                let d = self.drag.as_mut().expect("drag in progress");
                d.active = true;
                d.target = target;
                self.hover = None;
                self.cursor_icon = CursorIcon::Grabbing;
                self.dirty = true;
            }
            return;
        }
        self.update_hover(x, y);
    }

    /// What's under the mouse: hovered rows/buttons and the cursor shape.
    fn update_hover(&mut self, x: f32, y: f32) {
        let (mut hover, mut tab, mut file, mut agent, mut plus) = (None, None, None, None, false);
        let button = self.panel_button_at(x, y);
        let mut icon = CursorIcon::Default;
        let mut link = None;
        if self.prompt.is_none() {
            if button.is_some() {
                icon = CursorIcon::Pointer;
            } else if self.edge_at(x).is_some() {
                icon = CursorIcon::ColResize;
            } else if self.agents_edge_at(x, y) {
                icon = CursorIcon::RowResize;
            } else if self.files_rect().contains(x, y) {
                file = self.files_row_at(y);
                link = self.files_link_at(x, y);
            } else if self.agents_rect().contains(x, y) {
                agent = self.agent_row_at(y);
            } else if self.sidebar_plus().contains(x, y) {
                plus = true;
            } else if self.sidebar_rect().contains(x, y) {
                hover = self.sidebar_row_at(y);
            } else if self.pane_body_at(x, y).is_some() {
                icon = CursorIcon::Text;
            }
            tab = self.tab_at(x, y);
            if plus || hover.is_some() || file.is_some() || tab.is_some() || agent.is_some() {
                icon = CursorIcon::Pointer;
            }
        }
        if link != self.link_hover.map(|(i, _)| i) {
            self.link_hover = link.map(|i| (i, std::time::Instant::now()));
            self.dirty = true;
        }
        if (hover, tab, file, agent, button, plus)
            != (self.hover, self.tab_hover, self.files_hover, self.agents_hover, self.status_hover, self.plus_hover)
        {
            (self.hover, self.tab_hover, self.files_hover, self.agents_hover, self.status_hover, self.plus_hover) =
                (hover, tab, file, agent, button, plus);
            self.dirty = true;
        }
        self.set_cursor(icon);
    }

    pub fn on_mouse_button(&mut self, state: ElementState, b: MouseButton) {
        let (x, y) = self.mouse;
        self.dirty = true;
        if state == ElementState::Released {
            return self.on_release(x, y);
        }
        // An open menu takes the click: run the item, or dismiss.
        if self.menu.is_some() {
            let hit = self.menu_item_at(x, y);
            let m = self.menu.take().expect("menu is open");
            if let (Some(i), MouseButton::Left) = (hit, b) {
                self.run_action(m.items[i].1.clone());
            }
            return;
        }
        if self.finder.is_some() {
            return self.finder_click(b, x, y);
        }
        // A click away from the prompt (the status bar, or the sidebar row being typed in) cancels
        // it, like Esc, so it can't leave the rest of the window unresponsive.
        if let Some(inline) = self.prompt.as_ref().map(|p| p.inline) {
            let on_it = if inline {
                self.sidebar_rect().contains(x, y)
                    && self.sidebar_row_at(y).is_some_and(|i| matches!(self.rows()[i], Row::New(..)))
            } else {
                self.status_rect().contains(x, y)
            };
            if on_it && inline {
                return;
            }
            if !on_it {
                let p = self.prompt.take().expect("prompt is open");
                self.end_inline(&p);
            }
        }
        let free = self.prompt.is_none();
        if let Some(panel) = self.panel_button_at(x, y) {
            if b == MouseButton::Left {
                self.toggle_panel(panel);
                self.update_hover(x, y);
            }
            return;
        }
        let edge = self.edge_at(x);
        if free && edge.is_none() && self.agents_edge_at(x, y) {
            self.resizing = (b == MouseButton::Left).then_some(Edge::Agents);
            return;
        }
        if free && edge == Some(Edge::Files) {
            self.resizing = (b == MouseButton::Left).then_some(Edge::Files);
            return;
        }
        if free && self.files_rect().contains(x, y) {
            return self.files_click(b, x, y);
        }
        let in_sidebar = self.sidebar_rect().contains(x, y);
        if free && in_sidebar && b == MouseButton::Right && !self.agents_rect().contains(x, y) {
            return self.open_sidebar_menu(self.sidebar_row_at(y), x, y);
        }
        if edge == Some(Edge::Sidebar) {
            self.resizing = (b == MouseButton::Left).then_some(Edge::Sidebar);
            return;
        }
        if in_sidebar && self.agents_rect().contains(x, y) {
            if b == MouseButton::Left {
                self.agents_click(y);
                self.update_hover(x, y);
            }
            return;
        }
        if in_sidebar {
            if b == MouseButton::Left {
                self.sidebar_press(x, y);
            }
            return;
        }
        if self.tab_bar().contains(x, y) {
            return match (self.tab_at(x, y), b) {
                (Some(TabHit::New), MouseButton::Left) => self.new_tab(),
                (Some(TabHit::Close(i)), MouseButton::Left) => self.close_tab(i),
                (Some(TabHit::Tab(i)), MouseButton::Left) => self.select_tab(i as isize, false),
                (Some(h), MouseButton::Middle) => {
                    if let Some(i) = h.tab() {
                        self.close_tab(i);
                    }
                }
                _ => {}
            };
        }
        let Some(pr) = self.pane_at(x, y) else { return };
        if let Some(v) = self.current_view_mut() {
            v.reveal(pr.id);
        }
        self.focus_panes();
        if self.panes[&pr.id].editor.is_some() {
            if b == MouseButton::Left && pr.body.contains(x, y) {
                self.editor_press(pr.id, x, y);
            }
            return;
        }
        if self.panes[&pr.id].search.is_some() {
            if b == MouseButton::Left {
                self.search_press(pr.id, x, y);
            }
            return;
        }
        if !pr.body.contains(x, y) {
            return;
        }
        // Apps with mouse reporting get the click; Shift selects anyway (as in other terminals).
        if self.panes[&pr.id].vt.mode().intersects(TermMode::MOUSE_MODE) && !self.mods.shift_key() {
            self.mouse_pane = Some((pr.id, b));
            self.pane_mouse(pr.id, MouseKind::Down(b), x, y);
        } else if b == MouseButton::Left {
            self.term_select_press(pr.id, x, y);
        }
    }

    /// Grid point and cell side under (x, y) in terminal pane `id`, clamped to its screen.
    fn term_point(&self, id: u32, x: f32, y: f32) -> Option<(Point, Side)> {
        let pr = self.layout().into_iter().find(|p| p.id == id)?;
        let p = self.panes.get(&id)?;
        let (cols, rows) = (p.cols.max(1), p.rows.max(1));
        let fx = ((x - pr.body.x) / self.cell_w).max(0.0);
        let col = (fx.floor() as usize).min(cols - 1);
        let row = (((y - pr.body.y) / self.cell_h).floor().max(0.0) as usize).min(rows - 1);
        let side = if x >= pr.body.x + pr.body.w || fx.fract() >= 0.5 { Side::Right } else { Side::Left };
        let line = Line(row as i32 - p.vt.display_offset() as i32);
        Some((Point::new(line, Column(col)), side))
    }

    /// Left press on a terminal: start a selection (word on double click, line on triple).
    fn term_select_press(&mut self, id: u32, x: f32, y: f32) {
        let Some((pt, side)) = self.term_point(id, x, y) else { return };
        let now = Instant::now();
        let n = match self.term_click {
            Some((i, q, t, n)) if i == id && q == pt && now - t < DOUBLE_CLICK => n % 3 + 1,
            _ => 1,
        };
        self.term_click = Some((id, pt, now, n));
        let ty = [SelectionType::Simple, SelectionType::Semantic, SelectionType::Lines][n as usize - 1];
        let Some(p) = self.panes.get_mut(&id) else { return };
        let extend = n == 1 && self.mods.shift_key() && p.vt.term.selection.is_some();
        match &mut p.vt.term.selection {
            Some(s) if extend => s.update(pt, side),
            sel => *sel = Some(Selection::new(ty, pt, side)),
        }
        self.term_drag = Some(id);
    }

    fn term_select_drag(&mut self, id: u32, x: f32, y: f32) {
        let Some((pt, side)) = self.term_point(id, x, y) else { return };
        if let Some(s) = self.panes.get_mut(&id).and_then(|p| p.vt.term.selection.as_mut()) {
            s.update(pt, side);
            self.dirty = true;
        }
    }

    /// The focused terminal's selected text, if any.
    pub(super) fn term_selection_text(&self) -> Option<String> {
        let p = self.panes.get(&self.focused()?)?;
        if p.editor.is_some() || p.search.is_some() {
            return None;
        }
        p.vt.term.selection_to_string().filter(|s| !s.is_empty())
    }

    fn on_release(&mut self, x: f32, y: f32) {
        self.resizing = None;
        self.edit_drag = None;
        if let Some(id) = self.term_drag.take() {
            // A plain click leaves no selection behind.
            if let Some(p) = self.panes.get_mut(&id) {
                if p.vt.term.selection.as_ref().is_some_and(|s| s.is_empty()) {
                    p.vt.term.selection = None;
                }
            }
        }
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
    }

    /// Left press in the sidebar: a project's folder icon (fold), or a row.
    fn sidebar_press(&mut self, x: f32, y: f32) {
        if self.sidebar_plus().contains(x, y) {
            return self.add_home_workspace();
        }
        let Some(i) = self.sidebar_row_at(y) else { return };
        let row = self.rows()[i];
        self.sel = i;
        // A workspace's or project's folder icon folds it; the rest of the row opens it.
        let on_icon = || self.sidebar_tree(self.rows().len()).row(row.level(), i).icon.contains(x, y);
        let fold = match row {
            Row::Ws(w) if !self.workspaces[w].projects.is_empty() => Some(ws_key(&self.workspaces[w].name)),
            Row::Proj(w, p) if self.project(w, p).has_linked() => Some(proj_key(&self.project(w, p).path)),
            _ => None,
        };
        if let Some(k) = fold.filter(|_| on_icon()) {
            return self.toggle_fold(k);
        }
        // Workspace/project rows act on release, so they can be dragged to reorder.
        if matches!(row, Row::Ws(_) | Row::Proj(..)) {
            self.drag = Some(Drag { row, start_y: y, active: false, target: None });
            return;
        }
        self.click_row(row);
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
        if self.finder.is_some() {
            if self.finder_rect().contains(x, y) {
                self.finder_wheel(-n);
                self.finder_hover(x, y);
            }
            return;
        }
        if self.files_rect().contains(x, y) {
            self.files_scroll(n);
            return self.on_cursor_moved(x, y);
        }
        if self.agents_rect().contains(x, y) {
            self.agents_scroll(n);
            return self.update_hover(x, y);
        }
        if self.sidebar_rect().contains(x, y) {
            self.sidebar_scroll(n);
            return self.update_hover(x, y);
        }
        let Some(pr) = self.pane_body_at(x, y) else { return };
        if self.panes[&pr.id].editor.is_some() {
            return self.editor_wheel(pr.id, n);
        }
        if self.panes[&pr.id].search.is_some() {
            return self.search_wheel(pr.id, n);
        }
        let mode = self.panes[&pr.id].vt.mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            let kind = if n > 0 { MouseKind::WheelUp } else { MouseKind::WheelDown };
            for _ in 0..n.abs() {
                self.pane_mouse(pr.id, kind, x, y);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) {
            // Full-screen apps without mouse support: arrow keys, if they asked for that.
            if mode.contains(TermMode::ALTERNATE_SCROLL) {
                let k: &[u8] = if n > 0 { b"\x1b[A" } else { b"\x1b[B" };
                self.write_pane(pr.id, &k.repeat(n.unsigned_abs() as usize));
            }
        } else if let Some(p) = self.panes.get_mut(&pr.id) {
            p.vt.scroll(Scroll::Delta(n));
        }
    }
}
