//! Window geometry, in physical px: panels, tabs, panes, sidebar and explorer rows, hit testing.
use super::{Action, App, TabHit};

// Layout, in logical px (multiplied by the scale factor).
/// Sidebar default width.
pub const SIDEBAR_W: f32 = 250.0;
/// Tree row: Zed's project panel entry, a 24px (`h_6`) label box inside a 1px border.
pub const ROW_H: f32 = 26.0;
/// Workspaces tree rows: roomier than the other panels, so projects are easy to tell apart.
const SIDEBAR_ROW_H: f32 = 32.0;
/// Tree indent per level (Zed's project panel `indent_size`).
pub const INDENT: f32 = 20.0;
/// UI icon size, in points.
pub const ICON_PT: f32 = 14.0;
/// Symlink arrow size, Zed's indicator icon size.
pub const LINK_PT: f32 = 10.0;
/// Status bar height.
pub const STATUS_H: f32 = 26.0;
/// Padding between a pane's column and its cell grid.
pub const PAD: f32 = 8.0;
/// Tab bar height.
pub const TAB_H: f32 = 32.0;
/// Sidebar / explorer title header height (matches the tab bar).
pub const PANEL_HEADER_H: f32 = TAB_H;
/// Widest a tab gets in the tab bar.
pub const TAB_MAX_W: f32 = 200.0;
/// File explorer (right panel) default width.
pub const FILES_W: f32 = 260.0;
/// Narrowest a resizable panel gets.
const PANEL_MIN_W: f32 = 160.0;
/// How close to a panel edge (each side) the mouse grabs it.
const EDGE_GRAB: f32 = 3.0;
const MENU_W: f32 = 210.0;
const MENU_ITEM_H: f32 = 26.0;
const MENU_SEP_H: f32 = 9.0;
const MENU_PAD: f32 = 4.0;

/// A rectangle.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
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
    /// Shrunk by `d` on every side.
    pub fn inset(&self, d: f32) -> R {
        R { x: self.x + d, y: self.y + d, w: (self.w - 2.0 * d).max(0.0), h: (self.h - 2.0 * d).max(0.0) }
    }
    /// A `w`×`h` rect centered in this one, on whole pixels.
    pub fn center(&self, w: f32, h: f32) -> R {
        R { x: self.x + ((self.w - w) / 2.0).round(), y: self.y + ((self.h - h) / 2.0).round(), w, h }
    }
}

/// A scrolled tree panel (sidebar or explorer). Both panels share it, so they lay out, scroll and
/// hit-test rows identically.
#[derive(Clone, Copy, Debug)]
pub struct Tree {
    /// The panel's content rect (inside its border).
    pub area: R,
    /// First visible row.
    pub top: usize,
    /// Number of rows.
    pub len: usize,
    /// Row height, in logical pixels.
    pub(super) row_pt: f32,
    pub(super) scale: f32,
}

/// One row of a tree: full-width background, then the icon in its level's slot (an indent guide
/// runs through the slot's center, under the row's children), then the label. Right-aligned
/// extras end at `right`.
pub struct TreeRow {
    pub bg: R,
    /// The level slot; the icon is centered in it.
    pub icon: R,
    /// Where the label starts.
    pub text_x: f32,
    /// Where right-aligned content ends.
    pub right: f32,
}

impl Tree {
    fn px(&self, logical: f32) -> f32 {
        (logical * self.scale).round()
    }

    pub fn row_h(&self) -> f32 {
        self.px(self.row_pt)
    }

    /// Rows that fit.
    pub fn capacity(&self) -> usize {
        (self.area.h / self.row_h()).max(1.0) as usize
    }

    /// Rows on screen.
    pub fn visible(&self) -> std::ops::Range<usize> {
        self.top..(self.top + self.capacity()).min(self.len)
    }

    /// Screen y of row `i` (above the panel when scrolled past).
    pub fn y(&self, i: usize) -> f32 {
        self.area.y + (i as f32 - self.top as f32) * self.row_h()
    }

    /// Row `i`, at nesting `level`.
    pub fn row(&self, level: usize, i: usize) -> TreeRow {
        let (y, h) = (self.y(i), self.row_h());
        // Zed's list item: 6px side padding, 14px icon, 4px gap to the label.
        let icon = R { x: self.area.x + self.px(6.0) + level as f32 * self.px(INDENT), y, w: self.px(ICON_PT), h };
        TreeRow { bg: R { y, h, ..self.area }, text_x: icon.x + icon.w + self.px(4.0), icon, right: self.area.x + self.area.w - self.px(8.0) }
    }

    /// Row index at screen y, if there's a row there.
    pub fn index_at(&self, y: f32) -> Option<usize> {
        if y < self.area.y || y >= self.area.y + self.area.h {
            return None;
        }
        let i = self.top + ((y - self.area.y) / self.row_h()) as usize;
        (i < self.len).then_some(i)
    }

    /// Fractional row position at screen y (for drop targets).
    pub fn pos_at(&self, y: f32) -> f32 {
        self.top as f32 + (y - self.area.y) / self.row_h()
    }
}

/// Where a pane of the shown tab sits.
pub struct PaneRect {
    pub id: u32,
    /// The whole column: focus/click target.
    pub column: R,
    /// The cell grid.
    pub body: R,
}

impl App {
    pub fn px(&self, logical: f32) -> f32 {
        (logical * self.scale).round()
    }

    /// Width of 1px lines and separators.
    pub fn hairline(&self) -> f32 {
        self.scale.floor().max(1.0)
    }

    // ---- panels ----

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

    /// The status bar's panel toggles, left to right (none while a prompt has the bar).
    pub fn panel_buttons(&self) -> Vec<(super::Panel, R)> {
        if self.prompt.is_some() {
            return Vec::new();
        }
        let r = self.status_rect();
        let (line, w, gap) = (self.hairline(), self.px(STATUS_H - 2.0), self.px(2.0));
        let y = r.y + line + self.px(1.0);
        let h = r.h - line - self.px(2.0);
        let panels = [super::Panel::Sidebar, super::Panel::Agents, super::Panel::Files];
        panels.into_iter().enumerate().map(|(i, p)| (p, R { x: self.px(6.0) + i as f32 * (w + gap), y, w, h })).collect()
    }

    pub(super) fn panel_button_at(&self, x: f32, y: f32) -> Option<super::Panel> {
        self.panel_buttons().into_iter().find(|(_, r)| r.contains(x, y)).map(|(p, _)| p)
    }

    /// File explorer on the right; empty when hidden or no view is open.
    pub fn files_rect(&self) -> R {
        if !self.show_files || self.explorer().is_none() {
            return R::default();
        }
        let w = self.px(self.files_w).min((self.w - self.sidebar_rect().w - self.px(200.0)).max(0.0));
        R { x: self.w - w, y: 0.0, w, h: self.h - self.px(STATUS_H) }
    }

    /// Between the sidebar and the explorer, below the tab bar.
    pub fn pane_area(&self) -> R {
        let x = self.sidebar_rect().w;
        let top = self.tab_bar().h;
        R { x, y: top, w: (self.w - x - self.files_rect().w).max(0.0), h: (self.h - self.px(STATUS_H) - top).max(0.0) }
    }

    /// Sidebar content: the panel minus its right border.
    pub(super) fn sidebar_content(&self) -> R {
        let sb = self.sidebar_rect();
        R { w: (sb.w - self.hairline()).max(0.0), ..sb }
    }

    /// Explorer content: the panel minus its left border.
    fn files_content(&self) -> R {
        let f = self.files_rect();
        let line = if f.w > 0.0 { self.hairline() } else { 0.0 };
        R { x: f.x + line, w: (f.w - line).max(0.0), ..f }
    }

    /// A panel's title header: the top of its content, as tall as the tab bar so they line up.
    pub(super) fn header_of(&self, content: R) -> R {
        R { h: self.px(PANEL_HEADER_H).min(content.h), ..content }
    }

    /// Below a panel's header: where its tree goes.
    pub(super) fn below_header(&self, content: R) -> R {
        let h = self.header_of(content).h;
        R { y: content.y + h, h: (content.h - h).max(0.0), ..content }
    }

    /// The sidebar above the agents panel: workspace tree.
    fn workspaces_content(&self) -> R {
        let c = self.sidebar_content();
        R { h: (self.agents_rect().y - c.y).max(0.0), ..c }
    }

    pub fn sidebar_header(&self) -> R {
        self.header_of(self.workspaces_content())
    }

    pub fn files_header(&self) -> R {
        self.header_of(self.files_content())
    }

    pub(super) fn make_tree(&self, area: R, top: usize, len: usize) -> Tree {
        Tree { area, top, len, row_pt: ROW_H, scale: self.scale }
    }

    /// The sidebar's tree of `nrows` rows. Keeps its wheel scroll; when the selection changes it
    /// scrolls just enough to show it.
    pub fn sidebar_tree(&self, nrows: usize) -> Tree {
        let t = Tree { row_pt: SIDEBAR_ROW_H, ..self.make_tree(self.below_header(self.workspaces_content()), 0, nrows) };
        let cap = t.capacity();
        let sel = self.sel.min(nrows.saturating_sub(1));
        let mut top = self.side_top.get();
        if self.side_sel.replace(sel) != sel {
            top = top.min(sel).max((sel + 1).saturating_sub(cap));
        }
        top = top.min(nrows.saturating_sub(cap));
        self.side_top.set(top);
        Tree { top, ..t }
    }

    /// Wheel over the sidebar: scroll `n` rows (positive = up); a no-op when everything fits.
    pub(super) fn sidebar_scroll(&mut self, n: i32) {
        let len = self.rows().len();
        let t = self.sidebar_tree(len);
        let max = len.saturating_sub(t.capacity());
        self.side_top.set((t.top as i32 - n).clamp(0, max as i32) as usize);
    }

    /// The explorer's tree (empty when there's none).
    pub fn files_tree(&self) -> Tree {
        let (top, len) = self.explorer().map_or((0, 0), |e| (e.top, e.entries.len()));
        self.make_tree(self.below_header(self.files_content()), top, len)
    }

    /// The panel edge under x, if any.
    pub(super) fn edge_at(&self, x: f32) -> Option<super::Edge> {
        let grab = self.px(EDGE_GRAB);
        let f = self.files_rect();
        if self.show_sidebar && (x - self.sidebar_rect().w).abs() <= grab {
            Some(super::Edge::Sidebar)
        } else if f.w > 0.0 && (x - f.x).abs() <= grab {
            Some(super::Edge::Files)
        } else {
            None
        }
    }

    /// A panel width (logical px) for an edge dragged to `w` physical px.
    pub(super) fn panel_width(&self, w: f32) -> f32 {
        let max = (self.w / self.scale / 2.0).max(PANEL_MIN_W);
        (w / self.scale).clamp(PANEL_MIN_W, max)
    }

    // ---- tabs and panes ----

    /// Tab strip above the panes; zero height when no worktree is open.
    pub fn tab_bar(&self) -> R {
        let x = self.sidebar_rect().w;
        let h = if self.current.is_some() { self.px(TAB_H) } else { 0.0 };
        R { x, y: 0.0, w: (self.w - x - self.files_rect().w).max(0.0), h }
    }

    /// (tab, its × button) per tab of the current view, then the + (new tab) button.
    pub fn tab_rects(&self) -> (Vec<(R, R)>, R) {
        let bar = self.tab_bar();
        let n = self.current_view().map_or(0, |v| v.tabs.len());
        let plus_w = self.px(TAB_H);
        let w = if n == 0 { 0.0 } else { ((bar.w - plus_w) / n as f32).min(self.px(TAB_MAX_W)).floor() };
        let d = self.px(16.0);
        let tabs = (0..n)
            .map(|i| {
                let r = R { x: bar.x + i as f32 * w, w, ..bar };
                let close = R { x: r.x + r.w - d - self.px(8.0), w: d, ..r.center(d, d) };
                (r, close)
            })
            .collect();
        let p = self.px(22.0);
        let plus = R { x: bar.x + n as f32 * w, w: plus_w, ..bar }.center(p, p);
        (tabs, plus)
    }

    pub(super) fn tab_at(&self, x: f32, y: f32) -> Option<TabHit> {
        if !self.tab_bar().contains(x, y) {
            return None;
        }
        let (tabs, plus) = self.tab_rects();
        if plus.contains(x, y) {
            return Some(TabHit::New);
        }
        let i = tabs.iter().position(|(r, _)| r.contains(x, y))?;
        Some(if tabs[i].1.contains(x, y) { TabHit::Close(i) } else { TabHit::Tab(i) })
    }

    /// Current view's panes, side by side with 1px separators.
    pub fn layout(&self) -> Vec<PaneRect> {
        let Some(cur) = &self.current else { return vec![] };
        let ids = self.tab_panes(cur);
        let n = ids.len();
        if n == 0 {
            return vec![];
        }
        let a = self.pane_area();
        let sep = self.hairline();
        let colw = ((a.w - sep * (n - 1) as f32) / n as f32).floor();
        let pad = self.px(PAD);
        let mut x = a.x;
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                let w = if i == n - 1 { a.x + a.w - x } else { colw };
                let column = R { x, w, ..a };
                x += w + sep;
                PaneRect { id: *id, column, body: column.inset(pad) }
            })
            .collect()
    }

    /// The pane whose column is at (x, y), down to the window bottom.
    pub(super) fn pane_at(&self, x: f32, y: f32) -> Option<PaneRect> {
        self.layout().into_iter().find(|p| R { h: self.h, ..p.column }.contains(x, y))
    }

    /// The pane whose cell grid is at (x, y).
    pub(super) fn pane_body_at(&self, x: f32, y: f32) -> Option<PaneRect> {
        self.layout().into_iter().find(|p| p.body.contains(x, y))
    }

    /// (cols, rows) that fit in `body`.
    pub fn grid_size(&self, body: R) -> (usize, usize) {
        (((body.w / self.cell_w).floor() as usize).max(2), ((body.h / self.cell_h).floor() as usize).max(1))
    }

    /// Resize every shown pane's terminal to its rect.
    pub fn apply_layout(&mut self) {
        for pr in self.layout() {
            let (cols, rows) = self.grid_size(pr.body);
            let cell = (self.cell_w as u16, self.cell_h as u16);
            if let Some(p) = self.panes.get_mut(&pr.id) {
                p.resize(cols, rows, cell);
            }
        }
    }

    // ---- sidebar rows ----

    pub(super) fn sidebar_row_at(&self, y: f32) -> Option<usize> {
        self.sidebar_tree(self.rows().len()).index_at(y)
    }

    // ---- explorer rows ----

    pub fn files_visible_rows(&self) -> usize {
        self.files_tree().capacity()
    }

    pub(super) fn files_row_at(&self, y: f32) -> Option<usize> {
        self.files_tree().index_at(y)
    }

    /// The symlink arrow of explorer row `i` (in `t`), at the row's right end.
    pub fn files_link_icon(&self, t: &TreeRow) -> R {
        let w = self.px(LINK_PT);
        R { x: t.right - w, w, ..t.bg }
    }

    /// Explorer row whose symlink arrow is under (x, y).
    pub(super) fn files_link_at(&self, x: f32, y: f32) -> Option<usize> {
        let e = self.explorer()?;
        let i = self.files_row_at(y).filter(|i| e.entries[*i].link.is_some())?;
        let tree = self.files_tree();
        let r = self.files_link_icon(&tree.row(e.entries[i].depth, i));
        let pad = self.px(4.0);
        (x >= r.x - pad && x <= r.x + r.w + pad).then_some(i)
    }

    // ---- context menu ----

    fn menu_item_h(&self, a: &Action) -> f32 {
        self.px(if matches!(a, Action::Sep) { MENU_SEP_H } else { MENU_ITEM_H })
    }

    pub fn menu_size(&self, items: &[super::MenuItem]) -> (f32, f32) {
        let h: f32 = items.iter().map(|(_, a)| self.menu_item_h(a)).sum();
        (self.px(MENU_W), h + 2.0 * self.px(MENU_PAD))
    }

    /// Item rects of the open menu (separators included, as thin rows).
    pub fn menu_rects(&self) -> Vec<R> {
        let Some(m) = &self.menu else { return vec![] };
        let (w, _) = self.menu_size(&m.items);
        let pad = self.px(MENU_PAD);
        let mut y = m.y + pad;
        m.items
            .iter()
            .map(|(_, a)| {
                let h = self.menu_item_h(a);
                let r = R { x: m.x + pad, y, w: w - 2.0 * pad, h };
                y += h;
                r
            })
            .collect()
    }

    pub(super) fn menu_item_at(&self, x: f32, y: f32) -> Option<usize> {
        let m = self.menu.as_ref()?;
        self.menu_rects().iter().position(|r| r.contains(x, y)).filter(|i| !matches!(m.items[*i].1, Action::Sep))
    }
}

#[cfg(test)]
mod tests {
    use super::R;

    #[test]
    fn rect_helpers() {
        let r = R { x: 10.0, y: 10.0, w: 20.0, h: 10.0 };
        assert!(r.contains(10.0, 10.0) && !r.contains(30.0, 15.0));
        assert_eq!(r.inset(2.0), R { x: 12.0, y: 12.0, w: 16.0, h: 6.0 });
        assert_eq!(r.center(4.0, 4.0), R { x: 18.0, y: 13.0, w: 4.0, h: 4.0 });
    }
}
