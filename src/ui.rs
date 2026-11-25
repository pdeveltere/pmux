//! Build one frame's draw list: sidebar, tabs, terminal grids, file explorer, status bar, menu.
use crate::app::{ago, is_default_branch, proj_key, subtree_end, ws_key, Action, AgentRow, App, Focus, Group, Pane, Panel, PromptKind, Row, State, TabHit, Tree, TreeRow, R};
use crate::git::FileState;
use crate::gpu::{Inst, COLOR, MASK, RECT};
use crate::icons::{self, Icon};
use crate::text::{Fonts, Glyph};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

type C = [f32; 4];

const fn rgb(r: u8, g: u8, b: u8) -> C {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

pub const BG: C = rgb(0x1c, 0x1c, 0x20);
const SIDEBAR: C = rgb(0x15, 0x15, 0x18);
const HEADER_FOCUS: C = rgb(0x27, 0x2a, 0x33);
/// Panel headers and the tab bar: a band a step lighter than the panel below.
const BAND: C = rgb(0x22, 0x22, 0x28);
/// Hover on the band (tabs, the + button).
const BAND_HOVER: C = rgb(0x2a, 0x2a, 0x31);
const BORDER: C = rgb(0x2c, 0x2c, 0x32);
const TEXT: C = rgb(0xe4, 0xe4, 0xe7);
const MUTED: C = rgb(0x8b, 0x8b, 0x94);
const FAINT: C = rgb(0x5c, 0x5c, 0x66);
const ACCENT: C = rgb(0x5b, 0x9b, 0xff);
const HOVER: C = rgb(0x22, 0x22, 0x27);
const SELECT: C = rgb(0x26, 0x33, 0x4a);
/// Terminal selection: brighter than `SELECT`, so it shows over app backgrounds too.
const SELECT_TERM: C = rgb(0x3a, 0x4d, 0x70);
const MENU_BG: C = rgb(0x20, 0x20, 0x25);
const DANGER: C = rgb(0xf0, 0x6a, 0x6a);
/// ANSI red, as herdr colors its behind count.
const BEHIND: C = rgb(0xe0, 0x6c, 0x75);
const GUIDE: C = rgb(0x2e, 0x2e, 0x35);
const GUIDE_ACTIVE: C = rgb(0x52, 0x52, 0x5e);
/// ACCENT at 12% (premultiplied alpha).
const DRAG_TINT: C = [0x5b as f32 / 255.0 * 0.12, 0x9b as f32 / 255.0 * 0.12, 0.12, 0.12];
const CURSOR: C = rgb(0xd8, 0xd8, 0xd8);
/// Editor: the cursor's line.
const CURRENT_LINE: C = rgb(0x22, 0x22, 0x27);
/// Editor: a search match, Zed One Dark's #74ade8 at 40% (premultiplied alpha).
const MATCH: C = [0x74 as f32 / 255.0 * 0.4, 0xad as f32 / 255.0 * 0.4, 0xe8 as f32 / 255.0 * 0.4, 0.4];

const ANSI: [(u8, u8, u8); 16] = [
    (0x1c, 0x1c, 0x20),
    (0xe0, 0x6c, 0x75),
    (0x98, 0xc3, 0x79),
    (0xe5, 0xc0, 0x7b),
    (0x61, 0xaf, 0xef),
    (0xc6, 0x78, 0xdd),
    (0x56, 0xb6, 0xc2),
    (0xc8, 0xc8, 0xcc),
    (0x5c, 0x63, 0x70),
    (0xf0, 0x7f, 0x88),
    (0xb0, 0xd8, 0x8f),
    (0xf0, 0xd0, 0x8f),
    (0x7d, 0xc1, 0xff),
    (0xd7, 0x92, 0xea),
    (0x70, 0xcc, 0xd8),
    (0xff, 0xff, 0xff),
];

/// Default palette entry for xterm color index `i` (0-255, plus alacritty's named extras).
pub fn default_color(i: usize) -> Rgb {
    let (r, g, b) = match i {
        0..=15 => ANSI[i],
        16..=231 => {
            let n = i - 16;
            let v = |x: usize| if x == 0 { 0 } else { (55 + 40 * x) as u8 };
            (v(n / 36), v((n / 6) % 6), v(n % 6))
        }
        232..=255 => {
            let v = (8 + 10 * (i - 232)) as u8;
            (v, v, v)
        }
        _ if i == NamedColor::Background as usize => (0x1c, 0x1c, 0x20),
        _ if i == NamedColor::Cursor as usize => (0xd8, 0xd8, 0xd8),
        _ if i == NamedColor::DimForeground as usize => (0x9a, 0x9a, 0x9e),
        _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
            let (r, g, b) = ANSI[i - NamedColor::DimBlack as usize];
            ((r as u16 * 2 / 3) as u8, (g as u16 * 2 / 3) as u8, (b as u16 * 2 / 3) as u8)
        }
        _ => (0xdc, 0xdc, 0xde), // foreground, bright foreground
    };
    Rgb { r, g, b }
}

fn to_c(c: Rgb) -> C {
    rgb(c.r, c.g, c.b)
}

fn state_color(s: Option<State>) -> C {
    match s {
        Some(State::Working) => rgb(0xf5, 0xa5, 0x24),
        Some(State::Waiting | State::Error) => rgb(0xef, 0x5b, 0x5b),
        Some(State::Idle | State::Done) => rgb(0x4c, 0xc2, 0x6a),
        None => FAINT,
    }
}

fn group_color(g: Group) -> C {
    match g {
        Group::NeedsYou => state_color(Some(State::Waiting)),
        Group::Done => state_color(Some(State::Done)),
        Group::Working => state_color(Some(State::Working)),
        Group::Idle => MUTED,
    }
}

fn git_color(g: FileState) -> C {
    match g {
        FileState::Modified => rgb(0xe5, 0xc0, 0x7b),
        FileState::Added => rgb(0x98, 0xc3, 0x79),
        FileState::Deleted => BEHIND,
        FileState::Conflict => DANGER,
        FileState::Ignored => FAINT,
    }
}


/// A glyph to draw in a cell grid: char, style, x, baseline y, color.
type CellGlyph = (char, u8, f32, f32, C);

/// Buffers a frame is built in, kept between frames so drawing doesn't allocate them anew
/// (a full-screen terminal is thousands of entries).
#[derive(Default)]
pub struct FrameBufs {
    pub insts: Vec<Inst>,
    cells: Vec<CellGlyph>,
}

pub struct Painter<'a> {
    pub out: &'a mut Vec<Inst>,
    pub fonts: &'a mut Fonts,
    /// Scratch for a pane's glyphs, drawn after its backgrounds and cursor.
    cells: &'a mut Vec<CellGlyph>,
}

impl Painter<'_> {
    pub fn rect(&mut self, r: R, color: C) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.out.push(Inst { rect: [r.x, r.y, r.w, r.h], uv: [0.0; 4], color, misc: [RECT, 0.0, 0.0, 0.0] });
    }

    fn glyph(&mut self, g: Glyph, x: f32, y: f32, color: C) {
        let rect = [x + g.left as f32, y - g.top as f32, g.uv[2], g.uv[3]];
        self.out.push(Inst { rect, uv: g.uv, color, misc: [if g.color { COLOR } else { MASK }, 0.0, 0.0, 0.0] });
    }

    /// Draw text from `r.x`, vertically centered in `r`, clipped with an ellipsis at `r.w`. Returns width drawn.
    pub fn text(&mut self, s: &str, r: R, pt: f32, color: C, bold: bool) -> f32 {
        if r.w <= 0.0 || s.is_empty() {
            return 0.0;
        }
        let sh = self.fonts.ui(s, pt, bold, false);
        let lh = self.fonts.line_h(pt);
        let base = (r.y + ((r.h - lh) / 2.0).round() + sh.ascent.round()).round();
        let x = r.x.round();
        let clip = sh.width > r.w;
        let ell = if clip { self.fonts.ui("…", pt, bold, false) } else { sh.clone() };
        let limit = if clip { r.w - ell.width } else { f32::MAX };
        let mut end = 0.0;
        for &(key, gx, gy, adv) in &sh.glyphs {
            if adv > limit {
                break;
            }
            if let Some(g) = self.fonts.glyph(key) {
                self.glyph(g, x + gx as f32, base + gy as f32, color);
            }
            end = adv;
        }
        if !clip {
            return sh.width;
        }
        for &(key, gx, gy, _) in &ell.glyphs {
            if let Some(g) = self.fonts.glyph(key) {
                self.glyph(g, x + end + gx as f32, base + gy as f32, color);
            }
        }
        end + ell.width
    }

    /// Text centered in `r` (button labels, notices).
    fn text_centered(&mut self, s: &str, r: R, pt: f32, color: C) {
        let w = self.measure(s, pt, false);
        self.text(s, R { x: r.x + (r.w - w) / 2.0, ..r }, pt, color, false);
    }

    /// Text ending at `right`, never clipped. Returns its left edge.
    fn text_right(&mut self, s: &str, right: f32, r: R, pt: f32, color: C, bold: bool) -> f32 {
        let w = self.measure(s, pt, bold);
        self.text(s, R { x: right - w, w: w + 1.0, ..r }, pt, color, bold);
        right - w
    }

    pub fn measure(&mut self, s: &str, pt: f32, bold: bool) -> f32 {
        self.fonts.ui(s, pt, bold, false).width
    }

    /// Draw an icon centered in `r`.
    fn icon(&mut self, icon: &'static Icon, r: R, color: C) {
        self.icon_pt(icon, r, color, crate::app::ICON_PT);
    }

    /// An icon at `pt` points, centered in `r`.
    fn icon_pt(&mut self, icon: &'static Icon, r: R, color: C, pt: f32) {
        let Some(g) = self.fonts.icon(icon, pt) else { return };
        let b = r.center(g.uv[2], g.uv[3]);
        self.out.push(Inst { rect: [b.x, b.y, b.w, b.h], uv: g.uv, color, misc: [MASK, 0.0, 0.0, 0.0] });
    }

    fn dot(&mut self, cx: f32, cy: f32, d: f32, color: C) {
        let (x, y) = ((cx - d / 2.0).round(), (cy - d / 2.0).round());
        self.out.push(Inst { rect: [x, y, d, d], uv: [0.0; 4], color, misc: [RECT, d / 2.0, 0.0, 0.0] });
    }

    /// A 1px outline inside `r`.
    fn outline(&mut self, r: R, t: f32, color: C) {
        self.rect(R { h: t, ..r }, color);
        self.rect(R { y: r.y + r.h - t, h: t, ..r }, color);
        self.rect(R { w: t, ..r }, color);
        self.rect(R { x: r.x + r.w - t, w: t, ..r }, color);
    }
}

/// Build the frame into `bufs.insts` (replacing what was there).
pub fn frame(app: &App, fonts: &mut Fonts, bufs: &mut FrameBufs) {
    bufs.insts.clear();
    let mut p = Painter { out: &mut bufs.insts, fonts, cells: &mut bufs.cells };
    if app.show_sidebar {
        sidebar(app, &mut p);
    }
    panes(app, &mut p);
    tabs(app, &mut p);
    files(app, &mut p);
    status(app, &mut p);
    finder(app, &mut p);
    menu(app, &mut p);
    link_tooltip(app, &mut p);
}

/// Panel background and its border on the side facing the panes.
fn panel(app: &App, p: &mut Painter, r: R, border_right: bool) {
    let line = app.hairline();
    p.rect(r, SIDEBAR);
    p.rect(R { x: if border_right { r.x + r.w - line } else { r.x }, w: line, ..r }, BORDER);
}

/// Panel title header: a raised band with an uppercase, letter-spaced title and a bottom border
/// like the tab bar's. The band is tinted while the panel has keyboard focus.
fn panel_header(app: &App, p: &mut Painter, r: R, title: &str, focused: bool) {
    if r.h <= 0.0 {
        return;
    }
    let line = app.hairline();
    p.rect(R { h: r.h - line, ..r }, if focused { HEADER_FOCUS } else { BAND });
    p.rect(R { y: r.y + r.h - line, h: line, ..r }, BORDER);
    let pad = app.px(12.0);
    let (end, track) = (r.x + r.w - pad, app.px(1.2));
    let color = if focused { TEXT } else { MUTED };
    let mut x = r.x + pad;
    let mut buf = [0u8; 4];
    for ch in title.chars().flat_map(char::to_uppercase) {
        if x >= end {
            break;
        }
        x += p.text(ch.encode_utf8(&mut buf), R { x, w: end - x, h: r.h - line, ..r }, 10.5, color, true) + track;
    }
}

/// Row highlight, strongest first: selected in the focused panel, current (open / selected
/// elsewhere), hovered.
fn row_bg(selected: bool, current: bool, hover: bool) -> Option<C> {
    if selected {
        Some(SELECT)
    } else if current {
        Some(HEADER_FOCUS)
    } else if hover {
        Some(HOVER)
    } else {
        None
    }
}

/// What a tree row shows. Folders show open/closed folder icons; there are no chevrons.
struct RowLook {
    icon: &'static Icon,
    icon_color: C,
    label: String,
    label_color: C,
    bold: bool,
    /// The label is clipped here (right-aligned extras sit after it).
    label_end: f32,
}

/// Draw a tree panel (sidebar or explorer), in passes: row backgrounds, indent guides, rows.
/// `levels[i]` is row i's nesting; `bg(i)` its highlight; `active(parent)` whether a guide is
/// highlighted. `row` draws a row's right-aligned extras and says what the row shows.
fn draw_tree(
    app: &App,
    p: &mut Painter,
    tree: Tree,
    levels: &[usize],
    bg: impl Fn(usize) -> Option<C>,
    active: impl Fn(usize) -> bool,
    mut row: impl FnMut(&mut Painter, usize, &TreeRow) -> RowLook,
) {
    for i in tree.visible() {
        if let Some(c) = bg(i) {
            p.rect(tree.row(levels[i], i).bg, c);
        }
    }

    // Indent guides, one pass: a stack of parents; a parent's guide runs through its icon slot
    // and ends at the first row not nested in it.
    let vis = tree.visible();
    let guide = |p: &mut Painter, start: usize, stop: usize| {
        if stop <= start + 1 || stop <= vis.start || start >= vis.end {
            return;
        }
        let slot = tree.row(levels[start], start).icon;
        let x = (slot.x + slot.w / 2.0).floor();
        let y0 = (tree.y(start + 1) + app.px(2.0)).max(tree.area.y);
        let y1 = (tree.y(stop) - app.px(2.0)).min(tree.area.y + tree.area.h);
        if y1 > y0 {
            p.rect(R { x, y: y0, w: app.hairline(), h: y1 - y0 }, if active(start) { GUIDE_ACTIVE } else { GUIDE });
        }
    };
    let mut stack: Vec<usize> = Vec::new();
    for (i, lv) in levels.iter().enumerate() {
        while let Some(s) = stack.pop_if(|s| levels[*s] >= *lv) {
            guide(p, s, i);
        }
        stack.push(i);
    }
    while let Some(s) = stack.pop() {
        guide(p, s, levels.len());
    }

    for i in tree.visible() {
        let t = tree.row(levels[i], i);
        let look = row(p, i, &t);
        p.icon(look.icon, t.icon, look.icon_color);
        p.text(&look.label, R { x: t.text_x, w: look.label_end - t.text_x, ..t.bg }, 13.0, look.label_color, look.bold);
    }
}

/// Sidebar: workspaces → projects → worktrees, drawn like the explorer (Zed's project panel).
fn sidebar(app: &App, p: &mut Painter) {
    panel(app, p, app.sidebar_rect(), true);
    panel_header(app, p, app.sidebar_header(), "Workspaces", app.focus == Focus::Sidebar);
    // + opens a new workspace at ~.
    let plus = app.sidebar_plus();
    if plus.h > 0.0 {
        if app.plus_hover {
            p.rect(plus, BAND_HOVER);
        }
        p.text_centered("+", plus, 15.0, if app.plus_hover { TEXT } else { MUTED });
    }
    let rows = app.rows();
    let tree = app.sidebar_tree(rows.len());
    let levels: Vec<usize> = rows.iter().map(|r| r.level()).collect();
    let sel = app.sel.min(rows.len().saturating_sub(1));
    let current_row = rows.iter().position(|r| app.row_view(*r).is_some() && app.row_view(*r) == app.current);
    let selected = |i: usize| app.focus == Focus::Sidebar && i == sel;
    let bg = |i: usize| row_bg(selected(i), Some(i) == current_row, app.hover == Some(i));
    let active = |start: usize| current_row.is_some_and(|c| c > start && c < subtree_end(&rows, start));

    draw_tree(app, p, tree, &levels, bg, active, |p, i, t| {
        let current = Some(i) == current_row;
        let mut behind_of: Option<&std::path::Path> = None;
        let (label, icon, fg, bold, panes): (String, &'static Icon, C, bool, Vec<u32>) = match rows[i] {
            Row::Ws(wi) => {
                let ws = &app.workspaces[wi];
                let folded = app.folded.contains(&ws_key(&ws.name));
                // Its own folder's panes; folded, its projects' roll up into it too.
                let panes = if folded { app.panes.values().filter(|pn| app.names(&pn.view).0 == ws.name).map(|pn| pn.id).collect() } else { app.panes_in(&ws.path) };
                let open = !folded && !ws.projects.is_empty();
                (ws.name.clone(), folder(open), TEXT, true, panes)
            }
            Row::Proj(wi, pi) => {
                let pr = &app.workspaces[wi].projects[pi];
                let folded = app.folded.contains(&proj_key(&pr.path));
                let main_listed = pr.listed().any(|(_, w)| w.main);
                let panes: Vec<u32> = if folded {
                    pr.view_paths().iter().flat_map(|v| app.panes_in(v)).collect()
                } else if main_listed {
                    vec![]
                } else {
                    app.panes_in(&pr.path)
                };
                if !main_listed {
                    behind_of = Some(&pr.path);
                }
                (pr.name.clone(), folder(pr.has_linked() && !folded), TEXT, false, panes)
            }
            Row::Wt(wi, pi, ti) => {
                let w = &app.workspaces[wi].projects[pi].worktrees.as_ref().expect("row has worktrees")[ti];
                let panes = app.panes_in(&w.path);
                behind_of = Some(&w.path);
                let dim = panes.is_empty() || app.removing.contains(&w.path);
                (w.branch.clone(), &icons::BRANCH, if dim { MUTED } else { TEXT }, false, panes)
            }
            // The name of a new project / worktree being typed: an edit field over the row, like a
            // new folder in Zed's project panel.
            Row::New(..) => {
                let pr = app.prompt.as_ref().expect("a new row has its prompt");
                let icon = if matches!(pr.kind, PromptKind::NewWorktree(_)) { &icons::BRANCH } else { &icons::FOLDER };
                p.rect(t.bg, ACCENT);
                p.rect(t.bg.inset(app.hairline()), SIDEBAR);
                let cx = (t.text_x + p.measure(&pr.input, 13.0, false) + app.px(1.0)).min(t.right - app.px(2.0));
                p.rect(R { x: cx, y: t.bg.y + app.px(4.0), w: app.px(1.5), h: t.bg.h - app.px(8.0) }, TEXT);
                return RowLook { icon, icon_color: MUTED, label: pr.input.clone(), label_color: TEXT, bold: false, label_end: t.right - app.px(4.0) };
            }
        };
        let r = t.bg;
        // One state dot on the right: the most urgent of the row's panes.
        let (dd, gap) = (app.px(7.0), app.px(5.0));
        // Stateless panes (plain shells) get no dot.
        let st = panes.iter().filter_map(|id| app.panes[id].state()).max();
        let dots_w = if st.is_some() { dd + gap } else { 0.0 };
        if let Some(st) = st {
            p.dot(t.right - dd / 2.0, r.y + r.h / 2.0, dd, state_color(Some(st)));
        }
        let mut label_end = t.right - dots_w - app.px(4.0);
        // Behind upstream: red ↓N, left of the dots.
        if let Some(n) = behind_of.and_then(|v| app.behind.get(v)) {
            label_end = p.text_right(&format!("\u{2193}{n}"), label_end, r, 11.5, BEHIND, false) - app.px(6.0);
        }
        // Being deleted in the background; it leaves the tree once git is done.
        if behind_of.is_some_and(|v| app.removing.contains(v)) {
            label_end = p.text_right("deleting\u{2026}", label_end, r, 11.5, MUTED, false) - app.px(6.0);
        // Merged but not deleted yet (an agent is busy in it, or it has uncommitted files).
        } else if behind_of.is_some_and(|v| app.merged.contains_key(v)) {
            label_end = p.text_right("merged", label_end, r, 11.5, MUTED, false) - app.px(6.0);
        }
        RowLook { icon, icon_color: if current { ACCENT } else { MUTED }, label, label_color: if current { ACCENT } else { fg }, bold: bold || current, label_end }
    });

    // Reorder drag: the dragged row stays highlighted; a line marks where it lands.
    if let Some(d) = app.drag.as_ref().filter(|d| d.active) {
        if let Some(i) = rows.iter().position(|r| *r == d.row).filter(|i| tree.visible().contains(i)) {
            p.rect(tree.row(d.row.level(), i).bg, DRAG_TINT);
        }
        if let Some((_, b, lv)) = d.target {
            let th = app.px(2.0);
            let y = (tree.y(b) - th / 2.0).clamp(tree.area.y, tree.area.y + tree.area.h - th);
            let t = tree.row(lv, b);
            p.rect(R { x: t.icon.x, y, w: t.bg.x + t.bg.w - t.icon.x, h: th }, ACCENT);
        }
    }
    agents(app, p);
}

/// A pane title worth showing next to its location: set by `pmux title`, or the program's own
/// (minus leading spinner/glyphs), unless it's just a path, `user@host`, or the agent's name.
fn agent_title(pn: &Pane) -> Option<String> {
    if let Some(t) = &pn.title {
        return Some(t.clone());
    }
    let t = pn.osc_title.as_deref()?.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    let telling = !t.is_empty() && !t.starts_with(['~', '/']) && !t.contains('@') && Some(t.to_lowercase().as_str()) != pn.agent;
    telling.then(|| t.to_string())
}

/// Agents panel, under the workspace tree: Needs you / Done / Working / Idle sections.
fn agents(app: &App, p: &mut Painter) {
    let area = app.agents_rect();
    if area.h <= 0.0 {
        return;
    }
    let line = app.hairline();
    p.rect(R { h: line, ..area }, BORDER);
    let rows = app.agent_rows();
    let header = app.agents_header();
    panel_header(app, p, R { y: header.y + line, h: header.h - line, ..header }, "Agents", false);
    let tree = app.agents_tree(rows.len());
    if rows.is_empty() {
        let r = R { y: tree.area.y, h: tree.row_h(), ..tree.area };
        p.text("No agents running", R { x: r.x + app.px(12.0), w: r.w - app.px(24.0), ..r }, 12.0, FAINT, false);
        return;
    }
    let focused = app.focused();
    let dd = app.px(7.0);
    for i in tree.visible() {
        let t = tree.row(0, i);
        let r = t.bg;
        let left = r.x + app.px(12.0);
        match rows[i] {
            AgentRow::Gap => {}
            AgentRow::Header(g, n) => {
                let hover = app.agents_hover == Some(i);
                let w = p.text(g.label(), R { x: left, w: t.right - left, ..r }, 11.5, group_color(g), true);
                let folded = app.agent_group_folded(g);
                let c = if hover { TEXT } else { FAINT };
                p.text(&if folded { format!("{n} hidden") } else { n.to_string() }, R { x: left + w + app.px(6.0), w: app.px(80.0), ..r }, 11.5, c, false);
            }
            AgentRow::Agent(id) => {
                let Some(pn) = app.panes.get(&id) else { continue };
                let g = app.agent_group(id).unwrap_or(Group::Idle);
                let is_focus = focused == Some(id);
                let bg = row_bg(false, is_focus, app.agents_hover == Some(i));
                if let Some(c) = bg {
                    p.rect(r, c);
                }
                // Status on the right: a ring when idle, else a dot in the group's color.
                let (cx, cy) = (t.right - dd / 2.0, r.y + r.h / 2.0);
                p.dot(cx, cy, dd, group_color(g));
                if g == Group::Idle {
                    p.dot(cx, cy, dd - 2.0 * app.px(1.5), bg.unwrap_or(SIDEBAR));
                }
                let mut end = t.right - dd - app.px(8.0);
                end = p.text_right(&ago(pn.since()), end, r, 11.0, FAINT, false) - app.px(8.0);
                // "location · title": location is the project, or the branch of a linked worktree.
                let (_, proj, wt) = app.names(&pn.view);
                let place = if is_default_branch(&wt) || wt == proj { proj } else { wt };
                let dim = g == Group::Idle && !is_focus;
                let left = left + agent_icon(app, p, pn, left, r);
                let w = p.text(&place, R { x: left, w: (end - left).max(0.0), ..r }, 13.0, if dim { MUTED } else { TEXT }, false);
                let rest = agent_title(pn).unwrap_or_else(|| pn.kind().to_string());
                let x = left + w + app.px(5.0);
                if end > x {
                    p.text(&format!("· {rest}"), R { x, w: end - x, ..r }, 13.0, FAINT, false);
                }
            }
        }
    }
}

/// The pane's agent icon (its mark, in its color) at `x`, vertically centred in `r`.
/// Returns the width it took, gap included; 0 when the agent has no icon.
fn agent_icon(app: &App, p: &mut Painter, pn: &Pane, x: f32, r: R) -> f32 {
    let Some((icon, [cr, cg, cb])) = pn.agent.and_then(icons::agent) else { return 0.0 };
    let pt = 13.0;
    let w = app.px(pt);
    p.icon_pt(icon, R { x, w, ..r }, rgb(cr, cg, cb), pt);
    w + app.px(6.0)
}

fn folder(open: bool) -> &'static Icon {
    if open {
        &icons::FOLDER_OPEN
    } else {
        &icons::FOLDER
    }
}

/// File explorer on the right: the same tree as the sidebar, with file-type icons and git status colors.
fn files(app: &App, p: &mut Painter) {
    let fr = app.files_rect();
    let Some(e) = app.explorer().filter(|_| fr.w > 0.0) else { return };
    panel(app, p, fr, false);
    panel_header(app, p, app.files_header(), "Files", app.focus == Focus::Files);
    let levels: Vec<usize> = e.entries.iter().map(|en| en.depth).collect();
    let focused = app.focus == Focus::Files;
    let bg = |i: usize| row_bg(i == e.sel && focused, i == e.sel, app.files_hover == Some(i));
    let active_parent = e.parent(e.sel);
    draw_tree(app, p, app.files_tree(), &levels, bg, |start| active_parent == Some(start), |p, i, t| {
        let en = &e.entries[i];
        let git = e.git(&en.path);
        let label_color = git.map_or(TEXT, git_color);
        // Symlinks: Zed's arrow at indicator size on the right, in the label's color.
        let mut label_end = t.right;
        if en.link.is_some() {
            let r = app.files_link_icon(t);
            p.icon_pt(&icons::ARROW_UP_RIGHT, r, label_color, crate::app::LINK_PT);
            label_end = r.x - app.px(4.0);
        }
        RowLook {
            icon: if en.dir { folder(e.open.contains(&en.path)) } else { icons::files::for_name(&en.name) },
            icon_color: if git == Some(FileState::Ignored) { FAINT } else { MUTED },
            label: en.name.clone(),
            label_color,
            bold: en.depth == 0,
            label_end,
        }
    });
}

/// Shell titles are usually `user@host:~/dir`: the dir is the useful part.
fn short_title(label: String) -> String {
    match label.split_once(':') {
        Some((uh, rest)) if uh.contains('@') && !uh.contains(' ') && !rest.is_empty() => rest.trim().to_string(),
        _ => label,
    }
}

/// Tab strip over the panes: one tab per pane group, labelled by its focused pane, + at the end.
fn tabs(app: &App, p: &mut Painter) {
    let bar = app.tab_bar();
    let Some(v) = app.current_view() else { return };
    if bar.h <= 0.0 {
        return;
    }
    let line = app.hairline();
    p.rect(bar, BAND);
    p.rect(R { y: bar.y + bar.h - line, h: line, ..bar }, BORDER);
    let (rects, plus) = app.tab_rects();
    for (i, (t, (r, close))) in v.tabs.iter().zip(rects).enumerate() {
        let shown = i == v.tab;
        let hover = app.tab_hover.and_then(TabHit::tab) == Some(i);
        if shown {
            // The shown tab joins the pane area: pane background, no bottom border, accent on top.
            p.rect(r, BG);
            p.rect(R { h: app.px(2.0), ..r }, ACCENT);
        } else if hover {
            p.rect(R { h: r.h - line, ..r }, BAND_HOVER);
        }
        p.rect(R { x: r.x + r.w - line, w: line, h: r.h - if shown { 0.0 } else { line }, ..r }, BORDER);
        let fp = t.panes.get(t.focus).and_then(|id| app.panes.get(id));
        let mut label = fp.map(|pn| short_title(pn.label())).unwrap_or_default();
        if fp.and_then(|pn| pn.editor.as_ref()).is_some_and(|e| e.modified()) {
            label.push_str(" ●");
        }
        let mut x = r.x + app.px(12.0);
        if let Some(pn) = fp {
            x += agent_icon(app, p, pn, x, r);
        }
        let mut right = if hover || shown { close.x - app.px(4.0) } else { r.x + r.w - app.px(10.0) };
        // Split tab: pane count at the right end.
        if t.panes.len() > 1 {
            let n = t.panes.len().to_string();
            let nw = p.measure(&n, 11.0, false) + app.px(10.0);
            let nr = R { x: right - nw, w: nw, ..r.center(nw, app.px(16.0)) };
            p.rect(nr, BORDER);
            p.text(&n, R { x: nr.x + app.px(5.0), ..nr }, 11.0, MUTED, false);
            right = nr.x - app.px(6.0);
        }
        p.text(&label, R { x, w: (right - x).max(0.0), ..r }, 12.0, if shown { TEXT } else { MUTED }, false);
        if hover || shown {
            let on = app.tab_hover == Some(TabHit::Close(i));
            if on {
                p.rect(close, BORDER);
            }
            p.text_centered("×", close, 13.0, if on { TEXT } else { FAINT });
        }
    }
    let on = app.tab_hover == Some(TabHit::New);
    if on {
        p.rect(plus, BAND_HOVER);
    }
    p.text_centered("+", plus, 15.0, if on { TEXT } else { MUTED });
}

fn panes(app: &App, p: &mut Painter) {
    let area = app.pane_area();
    let layout = app.layout();
    if layout.is_empty() {
        let msg = if app.current.is_some() { "No panes. Press ⌘D to open one." } else { "Pick a worktree in the sidebar." };
        p.text_centered(msg, R { y: area.y + area.h / 2.0 - app.px(10.0), h: app.px(20.0), ..area }, 13.0, MUTED);
        return;
    }
    let focused = app.focused().filter(|_| app.focus == Focus::Panes && app.prompt.is_none() && app.finder.is_none());
    let sep = app.hairline();
    for (i, pr) in layout.iter().enumerate() {
        let pane = &app.panes[&pr.id];
        let is_focus = Some(pr.id) == focused;
        if i > 0 {
            p.rect(R { x: pr.column.x - sep, w: sep, ..area }, BORDER);
        }
        // No header: with several panes, a thin accent line marks the focused one.
        if is_focus && layout.len() > 1 {
            p.rect(R { h: app.px(2.0), ..pr.column }, ACCENT);
        }
        if let Some(ed) = &pane.editor {
            editor(app, p, ed, pr.body, is_focus);
            continue;
        }
        if let Some(s) = &pane.search {
            project_search(app, p, s, pr.body, is_focus);
            continue;
        }
        // Scrolled back: small badge in the top-right corner.
        let off = pane.vt.display_offset();
        if off > 0 {
            let s = format!("↑ {off}");
            let w = p.measure(&s, 11.0, false) + app.px(14.0);
            let b = R { x: pr.body.x + pr.body.w - w, y: pr.body.y, w, h: app.px(20.0) };
            p.rect(b, SELECT);
            p.text(&s, R { x: b.x + app.px(7.0), ..b }, 11.0, TEXT, false);
        }
        grid(app, p, pane, pr.body, is_focus);
    }
}

fn resolve(term: &alacritty_terminal::Term<crate::term::Listener>, c: Color) -> C {
    let indexed = |i: usize| to_c(term.colors()[i].unwrap_or_else(|| default_color(i)));
    match c {
        Color::Spec(rgb) => to_c(rgb),
        Color::Indexed(i) => indexed(i as usize),
        Color::Named(n) => indexed(n as usize),
    }
}

fn grid(app: &App, p: &mut Painter, pane: &Pane, body: R, focus: bool) {
    let term = &pane.vt.term;
    let g = term.grid();
    let (cw, ch) = (app.cell_w, app.cell_h);
    let base = p.fonts.baseline;
    let off = g.display_offset() as i32;
    let lines = g.screen_lines().min(pane.rows);
    let cols = g.columns().min(pane.cols);
    let default_bg = resolve(term, Color::Named(NamedColor::Background));
    let mode = *term.mode();
    let cur = g.cursor.point;
    let show_cursor = mode.contains(TermMode::SHOW_CURSOR) && off == 0;
    let thin = app.scale.max(1.0);
    let sel = term.selection.as_ref().and_then(|s| s.to_range(term));
    let mut glyphs = std::mem::take(p.cells);
    glyphs.clear();
    for l in 0..lines {
        let row = &g[Line(l as i32 - off)];
        let y = body.y + l as f32 * ch;
        // Background runs.
        let mut run: Option<(usize, C)> = None;
        for c in 0..=cols {
            let bg = (c < cols).then(|| {
                let cell = &row[Column(c)];
                resolve(term, if cell.flags.contains(Flags::INVERSE) { cell.fg } else { cell.bg })
            });
            let same = matches!((&run, &bg), (Some((_, a)), Some(b)) if a == b);
            if !same {
                if let Some((start, col)) = run.take().filter(|(_, col)| *col != default_bg) {
                    p.rect(R { x: body.x + start as f32 * cw, y, w: (c - start) as f32 * cw, h: ch }, col);
                }
                run = bg.map(|b| (c, b));
            }
        }
        // Selection: one run per line over the selected cells.
        if let Some(sel) = &sel {
            let line = Line(l as i32 - off);
            let inside = |c: usize| sel.contains(alacritty_terminal::index::Point::new(line, Column(c)));
            if let Some(a) = (0..cols).find(|&c| inside(c)) {
                let b = (a..cols).take_while(|&c| inside(c)).last().unwrap_or(a) + 1;
                p.rect(R { x: body.x + a as f32 * cw, y, w: (b - a) as f32 * cw, h: ch }, SELECT_TERM);
            }
        }
        for c in 0..cols {
            let cell = &row[Column(c)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN) {
                continue;
            }
            let mut fg = resolve(term, if cell.flags.contains(Flags::INVERSE) { cell.bg } else { cell.fg });
            if cell.flags.contains(Flags::DIM) {
                fg = [fg[0] * 0.66, fg[1] * 0.66, fg[2] * 0.66, 1.0];
            }
            let x = body.x + c as f32 * cw;
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                p.rect(R { x, y: y + base + app.px(2.0), w: cw, h: thin }, fg);
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                p.rect(R { x, y: y + (ch / 2.0).round(), w: cw, h: thin }, fg);
            }
            if cell.c != ' ' && cell.c != '\t' {
                let style = cell.flags.contains(Flags::BOLD) as u8 | (cell.flags.contains(Flags::ITALIC) as u8) << 1;
                glyphs.push((cell.c, style, x, y + base, fg));
            }
        }
    }
    // Cursor sits between backgrounds and text: a block when focused, an outline otherwise.
    let cur_l = cur.line.0;
    let mut cursor_cell = None;
    if show_cursor && cur_l >= 0 && (cur_l as usize) < lines && cur.column.0 < cols {
        let r = R { x: body.x + cur.column.0 as f32 * cw, y: body.y + cur_l as f32 * ch, w: cw, h: ch };
        if focus {
            p.rect(r, CURSOR);
            cursor_cell = Some((r.x, r.y + base));
        } else {
            p.outline(r, thin, FAINT);
        }
    }
    for &(c, style, x, y, fg) in &glyphs {
        let fg = if cursor_cell == Some((x, y)) { BG } else { fg };
        if let Some((gl, gx, gy)) = p.fonts.cell(c, style) {
            p.glyph(gl, x + gx as f32, y + gy as f32, fg);
        }
    }
    *p.cells = glyphs;
}

/// Color and font style (bit 0 bold, bit 1 italic) for a highlight, after Zed's One Dark.
fn syntax_style(k: u8) -> (C, u8) {
    let name = crate::syntax::NAMES[k as usize];
    let c = match name {
        "keyword" => rgb(0xb4, 0x77, 0xcf),
        "string" | "text.literal" | "markup.raw" => rgb(0xa1, 0xc1, 0x81),
        "string.escape" => rgb(0x87, 0x8e, 0x98),
        "string.special" | "number" | "boolean" | "constant" | "constant.builtin" | "variable.builtin" => rgb(0xbf, 0x95, 0x6a),
        "comment" | "comment.doc" => rgb(0x5d, 0x63, 0x6f),
        "function" | "function.builtin" | "function.macro" | "function.method" | "constructor" | "attribute" | "tag" | "label" => rgb(0x73, 0xad, 0xe9),
        "type" | "type.builtin" | "operator" | "module" | "text.uri" | "markup.link" | "text.reference" => rgb(0x6e, 0xb4, 0xbf),
        "property" | "text.title" | "markup.heading" => rgb(0xd0, 0x72, 0x77),
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter" | "punctuation.special" => rgb(0xb2, 0xb9, 0xc6),
        "escape" => rgb(0x87, 0x8e, 0x98),
        _ => TEXT,
    };
    let style = match name {
        "text.title" | "markup.heading" | "text.strong" | "markup.bold" => 1,
        "text.emphasis" | "markup.italic" => 2,
        _ => 0,
    };
    (c, style)
}

/// An editor pane: line numbers, current-line highlight, selection, text, bar cursor.
fn editor(app: &App, p: &mut Painter, ed: &crate::editor::Editor, body: R, focus: bool) {
    use crate::editor::Pos;
    let g = app.editor_geom(body, ed);
    let (cw, ch) = (app.cell_w, app.cell_h);
    let base = p.fonts.baseline;
    let sel = ed.selection();
    let matches = ed.search.as_ref().map_or(&[][..], |s| &s.matches[..]);
    let end = (ed.top + g.rows + 1).min(ed.lines.len());
    let mut glyphs = std::mem::take(p.cells);
    glyphs.clear();
    for l in ed.top..end {
        let y = g.text.y + (l - ed.top) as f32 * ch;
        if y >= body.y + body.h {
            break;
        }
        let here = l == ed.cur.line;
        if here && sel.is_none() {
            p.rect(R { y, h: ch, w: g.gutter.w + g.text.w, ..g.gutter }, CURRENT_LINE);
        }
        // Search matches on this line (the selected one shows as the selection).
        let first = matches.partition_point(|(a, _)| a.line < l);
        for (a, b) in matches[first..].iter().take_while(|(a, _)| a.line == l) {
            let (x0, x1) = (ed.x_of(*a), ed.x_of(*b));
            let (x0, x1) = (x0.max(ed.left) - ed.left, x1.saturating_sub(ed.left).min(g.cols));
            if x1 > x0 {
                p.rect(R { x: g.text.x + x0 as f32 * cw, y, w: (x1 - x0) as f32 * cw, h: ch }, MATCH);
            }
        }
        // Selection on this line: [a, b) in display cells, plus one cell for the newline.
        if let Some((a, b)) = sel.filter(|(a, b)| (a.line..=b.line).contains(&l)) {
            let x0 = if l == a.line { ed.x_of(a) } else { 0 };
            let x1 = if l == b.line { ed.x_of(b) } else { ed.x_of(Pos::new(l, usize::MAX)) + 1 };
            let (x0, x1) = (x0.max(ed.left) - ed.left, x1.saturating_sub(ed.left).min(g.cols));
            if x1 > x0 {
                p.rect(R { x: g.text.x + x0 as f32 * cw, y, w: (x1 - x0) as f32 * cw, h: ch }, SELECT);
            }
        }
        // Line number, right-aligned with two cells before the text.
        let num = (l + 1).to_string();
        let nx = g.gutter.x + g.gutter.w - (num.len() as f32 + 2.0) * cw;
        for (i, c) in num.chars().enumerate() {
            glyphs.push((c, 0, nx + i as f32 * cw, y + base, if here { MUTED } else { FAINT }));
        }
        let spans = ed.hl.get(l).map(Vec::as_slice).unwrap_or(&[]);
        let mut si = 0;
        let mut x = 0usize;
        for (col, c) in ed.lines[l].chars().enumerate() {
            while si < spans.len() && spans[si].1 <= col {
                si += 1;
            }
            let (fg, style) = spans.get(si).filter(|s| s.0 <= col).map_or((TEXT, 0), |s| syntax_style(s.2));
            let w = if c == '\t' { crate::editor::TAB - x % crate::editor::TAB } else { 1 };
            if x >= ed.left + g.cols {
                break;
            }
            if x >= ed.left && c != ' ' && c != '\t' {
                glyphs.push((c, style, g.text.x + (x - ed.left) as f32 * cw, y + base, fg));
            }
            x += w;
        }
    }
    for &(c, style, x, y, fg) in &glyphs {
        if let Some((gl, gx, gy)) = p.fonts.cell(c, style) {
            p.glyph(gl, x + gx as f32, y + gy as f32, fg);
        }
    }
    *p.cells = glyphs;
    let finding = ed.search.as_ref().is_some_and(|s| s.focused);
    if let (Some(bar), Some(s)) = (g.bar, &ed.search) {
        search_bar(app, p, ed, s, bar, focus);
    }
    // Bar cursor; hidden when the pane or the find bar has the keyboard (as in Zed).
    let cx = ed.x_of(ed.cur);
    if focus && !finding && ed.cur.line >= ed.top && cx >= ed.left && cx <= ed.left + g.cols {
        let y = g.text.y + (ed.cur.line - ed.top) as f32 * ch;
        if y < body.y + body.h {
            p.rect(R { x: g.text.x + (cx - ed.left) as f32 * cw, y, w: app.px(2.0), h: ch }, ACCENT);
        }
    }
}

/// The find bar over an editor: query field, match-case toggle, match count.
fn search_bar(app: &App, p: &mut Painter, ed: &crate::editor::Editor, s: &crate::editor::Search, bar: R, focus: bool) {
    let line = app.hairline();
    p.rect(bar, SIDEBAR);
    p.rect(R { y: bar.y + bar.h - line, h: line, ..bar }, BORDER);
    let pad = app.px(8.0);
    let field = R { x: bar.x + pad, y: bar.y + app.px(6.0), w: app.px(320.0).min(bar.w * 0.6), h: bar.h - app.px(12.0) };
    let active = focus && s.focused;
    let none = !s.query.is_empty() && s.matches.is_empty();
    p.rect(field, BG);
    p.outline(field, line, if none { DANGER } else if active { ACCENT } else { BORDER });
    let tx = field.x + app.px(8.0);
    let tr = R { x: tx, w: field.w - app.px(16.0), ..field };
    let w = if s.query.is_empty() { p.text("Search…", tr, 13.0, FAINT, false).min(0.0) } else { p.text(&s.query, tr, 13.0, TEXT, false) };
    if active {
        p.rect(R { x: (tx + w).min(field.x + field.w - app.px(6.0)), y: field.y + app.px(5.0), w: app.px(1.5), h: field.h - app.px(10.0) }, TEXT);
    }
    // Match case: highlighted when on (⌥⌘C).
    let aa = R { x: field.x + field.w + app.px(6.0), w: app.px(26.0), ..field };
    if s.case {
        p.rect(aa, SELECT);
    }
    p.text_centered("Aa", aa, 12.0, if s.case { TEXT } else { MUTED });
    let count = match (s.query.is_empty(), s.matches.len()) {
        (true, _) => String::new(),
        (false, 0) => "No results".into(),
        (false, n) => format!("{}/{n}", ed.match_index().map_or("?".into(), |i| i.to_string())),
    };
    p.text(&count, R { x: aa.x + aa.w + app.px(10.0), w: (bar.x + bar.w - aa.x - aa.w - app.px(18.0)).max(0.0), ..field }, 12.0, MUTED, false);
}

/// A project search tab (⌘⇧F): query bar with Zed's toggles, then each file's matching lines.
fn project_search(app: &App, p: &mut Painter, s: &crate::search::ProjectSearch, body: R, focus: bool) {
    use crate::search::Row;
    let g = app.search_geom(body);
    let line = app.hairline();
    let bar = g.bar;
    p.rect(bar, SIDEBAR);
    p.rect(R { y: bar.y + bar.h - line, h: line, ..bar }, BORDER);
    let field = g.field;
    p.rect(field, BG);
    p.outline(field, line, if s.error.is_some() { DANGER } else if focus { ACCENT } else { BORDER });
    let tx = field.x + app.px(8.0);
    let tr = R { x: tx, w: field.w - app.px(16.0), ..field };
    let w = if s.query.text.is_empty() { p.text("Search all files…", tr, 13.0, FAINT, false).min(0.0) } else { p.text(&s.query.text, tr, 13.0, TEXT, false) };
    if focus {
        p.rect(R { x: (tx + w).min(field.x + field.w - app.px(6.0)), y: field.y + app.px(5.0), w: app.px(1.5), h: field.h - app.px(10.0) }, TEXT);
    }
    for (t, (label, on)) in g.toggles.iter().zip([("Aa", s.query.case), ("ab", s.query.word), (".*", s.query.regex)]) {
        if on {
            p.rect(*t, SELECT);
        }
        p.text_centered(label, *t, 12.0, if on { TEXT } else { MUTED });
        if label == "ab" {
            // Whole word: a bar under the letters.
            p.rect(R { x: t.x + app.px(8.0), y: t.y + t.h - app.px(6.0), w: t.w - app.px(16.0), h: line.max(1.0) }, if on { TEXT } else { MUTED });
        }
    }
    let last = g.toggles[2];
    let info = if let Some(e) = &s.error {
        e.clone()
    } else if s.running.is_some() {
        "Searching…".into()
    } else if let (Some(f), Some(_)) = (&s.found, &s.searched) {
        let (n, files) = (s.hits, f.files.len());
        if n == 0 {
            "No results".into()
        } else {
            format!("{n}{} result{} in {files} file{}", if f.truncated { "+" } else { "" }, if n == 1 { "" } else { "s" }, if files == 1 { "" } else { "s" })
        }
    } else if s.searched.as_ref() != Some(&s.query) && !s.query.text.is_empty() {
        "Enter to search".into()
    } else {
        String::new()
    };
    let ix = last.x + last.w + app.px(10.0);
    p.text(&info, R { x: ix, w: (bar.x + bar.w - ix - app.px(8.0)).max(0.0), ..field }, 12.0, if s.error.is_some() { DANGER } else { MUTED }, false);

    let Some(found) = &s.found else { return };
    let (cw, ch) = (app.cell_w, app.cell_h);
    let base = p.fonts.baseline;
    let mut glyphs = std::mem::take(p.cells);
    glyphs.clear();
    let pad = app.px(10.0);
    let gutter = 6.0 * cw;
    for (k, row) in s.rows.iter().enumerate().skip(s.top).take(g.rows() + 1) {
        let y = g.list.y + (k - s.top) as f32 * g.row_h;
        if y >= g.list.y + g.list.h {
            break;
        }
        let r = R { x: g.list.x, y, w: g.list.w, h: g.row_h };
        match *row {
            Row::File(i) => {
                let fh = &found.files[i];
                let (dir, name) = fh.path.rsplit_once('/').unwrap_or(("", &fh.path));
                let ic = app.px(16.0);
                if k > 0 {
                    p.rect(R { y: r.y, h: line, ..r }, BORDER);
                }
                p.icon(icons::files::for_name(name), R { x: r.x + pad, w: ic, ..r }, MUTED);
                let x = r.x + pad + ic + app.px(8.0);
                let cnt = fh.hits.len().to_string();
                let right = r.x + r.w - pad;
                let cl = p.text_right(&cnt, right, r, 11.0, FAINT, false);
                let nw = p.text(name, R { x, w: (cl - x - app.px(8.0)).max(0.0), ..r }, 13.0, TEXT, true);
                let dx = x + nw + app.px(8.0);
                if !dir.is_empty() && dx < cl - app.px(8.0) {
                    p.text(dir, R { x: dx, w: cl - app.px(8.0) - dx, ..r }, 12.0, MUTED, false);
                }
            }
            Row::Hit(i, j) => {
                let h = &found.files[i].hits[j];
                let sel = k == s.sel;
                if sel {
                    p.rect(r, if focus { SELECT } else { HOVER });
                }
                let ty = y + ((g.row_h - ch) / 2.0).round();
                let num = (h.line + 1).to_string();
                let nx = r.x + pad + gutter - (num.len() as f32 + 1.0) * cw;
                for (n, c) in num.chars().enumerate() {
                    glyphs.push((c, 0, nx + n as f32 * cw, ty + base, if sel { MUTED } else { FAINT }));
                }
                let tx0 = r.x + pad + gutter + cw;
                let cols = ((r.x + r.w - pad - tx0) / cw).floor().max(0.0) as usize;
                // Leading indentation dropped, so lines from deep inside blocks still read.
                let skip = h.text.chars().take_while(|c| c.is_whitespace()).count();
                let mut x = 0usize;
                let mut xs = Vec::with_capacity(h.text.len());
                for (n, c) in h.text.chars().enumerate() {
                    xs.push(x);
                    if n < skip {
                        continue;
                    }
                    let w = if c == '\t' { crate::editor::TAB - x % crate::editor::TAB } else { 1 };
                    if x + w > cols {
                        break;
                    }
                    if c != ' ' && c != '\t' {
                        let hit = h.ranges.iter().any(|&(a, b)| n >= a && n < b);
                        glyphs.push((c, 0, tx0 + x as f32 * cw, ty + base, if hit { TEXT } else { MUTED }));
                    }
                    x += w;
                }
                xs.push(x);
                for &(a, b) in &h.ranges {
                    let (x0, x1) = (xs.get(a.max(skip)).copied().unwrap_or(x), xs.get(b).copied().unwrap_or(x).min(cols));
                    if x1 > x0 {
                        p.rect(R { x: tx0 + x0 as f32 * cw, y: ty, w: (x1 - x0) as f32 * cw, h: ch }, MATCH);
                    }
                }
            }
        }
    }
    for &(c, style, x, y, fg) in &glyphs {
        if let Some((gl, gx, gy)) = p.fonts.cell(c, style) {
            p.glyph(gl, x + gx as f32, y + gy as f32, fg);
        }
    }
    *p.cells = glyphs;
    if s.rows.is_empty() && s.searched.is_some() && s.running.is_none() {
        let r = R { y: g.list.y + app.px(20.0), h: app.px(20.0), ..g.list };
        p.text_centered("No results", r, 13.0, MUTED);
    }
}

/// Draw `s` from `r.x` with the chars at `hits` (char indices, offset by `from`) in ACCENT.
/// Returns the width drawn.
fn highlighted(p: &mut Painter, s: &str, from: usize, hits: &[usize], r: R, pt: f32, color: C) -> f32 {
    let mut x = r.x;
    let mut run = String::new();
    let mut run_hit = false;
    let flush = |p: &mut Painter, run: &mut String, hit: bool, x: &mut f32| {
        if !run.is_empty() {
            *x += p.text(run, R { x: *x, w: (r.x + r.w - *x).max(0.0), ..r }, pt, if hit { ACCENT } else { color }, false);
            run.clear();
        }
    };
    for (i, c) in s.chars().enumerate() {
        let hit = hits.contains(&(from + i));
        if hit != run_hit {
            flush(p, &mut run, run_hit, &mut x);
            run_hit = hit;
        }
        run.push(c);
    }
    flush(p, &mut run, run_hit, &mut x);
    x - r.x
}

/// "Go to file" popup (⌘P), like Zed's file finder: query on top, matches below.
fn finder(app: &App, p: &mut Painter) {
    let Some(f) = &app.finder else { return };
    let r = app.finder_rect();
    let b = app.hairline();
    p.rect(R { x: r.x - app.px(2.0), y: r.y + app.px(4.0), w: r.w + app.px(4.0), h: r.h + app.px(4.0) }, [0.0, 0.0, 0.0, 0.35]);
    p.rect(r, BORDER);
    p.rect(r.inset(b), MENU_BG);

    let input = app.finder_input();
    p.rect(R { y: input.y + input.h, h: b, ..input }, BORDER);
    let pad = app.px(12.0);
    let ir = R { x: input.x + pad, w: input.w - 2.0 * pad, ..input };
    let tw = if f.query.is_empty() {
        p.text("Search project files...", ir, 14.0, FAINT, false);
        0.0
    } else {
        p.text(&f.query, ir, 14.0, TEXT, false)
    };
    let ch = app.px(18.0);
    p.rect(R { x: ir.x + tw + app.px(1.0), y: input.y + ((input.h - ch) / 2.0).round(), w: app.px(1.5), h: ch }, ACCENT);

    if f.matches.is_empty() {
        let msg = if f.loading {
            "Indexing files…"
        } else if f.files.is_empty() {
            "No files"
        } else {
            "No matches"
        };
        let row = app.finder_row(0);
        p.text(msg, R { x: row.x + app.px(8.0), w: row.w - app.px(16.0), ..row }, 13.0, MUTED, false);
        return;
    }
    for (k, i) in (f.top..f.matches.len()).take(app.finder_rows()).enumerate() {
        let row = app.finder_row(k);
        if i == f.sel {
            p.rect(row, SELECT);
        } else if f.hover == Some(i) {
            p.rect(row, HOVER);
        }
        let m = &f.matches[i];
        let path = &f.files[m.file];
        let (dir, name) = path.rsplit_once('/').map_or(("", path.as_str()), |(d, n)| (d, n));
        let name_at = path.chars().count() - name.chars().count();
        let ic = app.px(16.0);
        p.icon(icons::files::for_name(name), R { x: row.x + app.px(8.0), w: ic, ..row }, MUTED);
        let tx = row.x + app.px(8.0) + ic + app.px(8.0);
        let end = row.x + row.w - app.px(8.0);
        let nw = highlighted(p, name, name_at, &m.pos, R { x: tx, w: end - tx, ..row }, 13.0, TEXT);
        let dx = tx + nw + app.px(8.0);
        if !dir.is_empty() && dx < end {
            highlighted(p, dir, 0, &m.pos, R { x: dx, w: end - dx, ..row }, 12.0, MUTED);
        }
    }
}

/// Right-click context menu, drawn on top of everything.
fn menu(app: &App, p: &mut Painter) {
    let Some(m) = &app.menu else { return };
    let (w, h) = app.menu_size(&m.items);
    let r = R { x: m.x, y: m.y, w, h };
    // Soft shadow, border, body.
    p.rect(R { x: r.x - app.px(1.0), y: r.y + app.px(2.0), w: r.w + app.px(2.0), h: r.h + app.px(2.0) }, [0.0, 0.0, 0.0, 0.35]);
    p.rect(r, BORDER);
    let b = app.hairline();
    p.rect(r.inset(b), MENU_BG);
    for (i, (ir, (label, a))) in app.menu_rects().into_iter().zip(&m.items).enumerate() {
        if matches!(a, Action::Sep) {
            p.rect(R { x: ir.x + app.px(4.0), y: ir.y + (ir.h / 2.0).floor(), w: ir.w - app.px(8.0), h: b }, BORDER);
            continue;
        }
        if m.hover == Some(i) {
            p.rect(ir, SELECT);
        }
        let danger = matches!(a, Action::Delete(_));
        p.text(label, R { x: ir.x + app.px(10.0), w: ir.w - app.px(20.0), ..ir }, 13.0, if danger { DANGER } else { TEXT }, false);
    }
}

/// Zed's symlink tooltip: the link's target, then "Symbolic Link" muted, below the mouse.
fn link_tooltip(app: &App, p: &mut Painter) {
    let Some(target) = app.link_tooltip().and_then(|i| app.explorer()?.entries.get(i)?.link.as_ref()) else { return };
    let title = target.display().to_string();
    let (pad, line_h) = (app.px(8.0), app.px(18.0));
    let w = p.measure(&title, 13.0, false).max(p.measure("Symbolic Link", 12.0, false)) + 2.0 * pad;
    let h = 2.0 * line_h + 2.0 * app.px(5.0);
    let (mx, my) = app.mouse;
    let x = mx.min(app.w - w - app.px(4.0)).max(0.0);
    let y = if my + app.px(18.0) + h <= app.h { my + app.px(18.0) } else { (my - app.px(6.0) - h).max(0.0) };
    let r = R { x, y, w, h };
    p.rect(R { x: r.x - app.px(1.0), y: r.y + app.px(2.0), w: r.w + app.px(2.0), h: r.h + app.px(2.0) }, [0.0, 0.0, 0.0, 0.35]);
    p.rect(r, BORDER);
    p.rect(r.inset(app.hairline()), MENU_BG);
    let row = |k: f32| R { x: r.x + pad, y: r.y + app.px(5.0) + k * line_h, w: r.w - 2.0 * pad + 1.0, h: line_h };
    p.text(&title, row(0.0), 13.0, TEXT, false);
    p.text("Symbolic Link", row(1.0), 12.0, MUTED, false);
}

fn status(app: &App, p: &mut Painter) {
    let r = app.status_rect();
    p.rect(r, SIDEBAR);
    p.rect(R { h: app.hairline(), ..r }, BORDER);
    let x = app.px(14.0);
    if let Some(pr) = app.prompt.as_ref().filter(|p| !p.inline) {
        let lw = p.text(&format!("{}:", pr.label), R { x, ..r }, 12.0, ACCENT, true);
        let ix = x + lw + app.px(8.0);
        let iw = p.text(&pr.input, R { x: ix, ..r }, 12.0, TEXT, false);
        p.rect(R { x: ix + iw + app.px(1.0), y: r.y + app.px(6.0), w: app.px(1.5), h: r.h - app.px(12.0) }, TEXT);
        p.text_right("Enter confirm · Esc cancel · Tab complete", r.w - x, r, 12.0, FAINT, false);
        return;
    }
    // Shortcut hints only when asked for (settings.toml); the prefix mode always says it's on.
    let inline = app.prompt.as_ref().filter(|p| p.inline);
    let right = if let Some(pr) = inline {
        if matches!(pr.kind, PromptKind::AddProject(_)) { "Enter create · Esc cancel · Tab complete" } else { "Enter create · Esc cancel" }
    } else if !app.settings.shortcut_hints {
        if app.prefix { "C-a" } else { "" }
    } else if app.finder.is_some() {
        "↑↓ select · Enter open · Esc close"
    } else if app.prefix {
        "| split · t tab · 1-9 tab · h/l focus · x close · c clear · j/k worktree · n attention · Tab sidebar · b/a/f hide panels · q quit"
    } else {
        let fed = app.focused().filter(|_| app.focus == Focus::Panes).and_then(|id| app.panes.get(&id)?.editor.as_ref());
        let editing = fed.is_some();
        let finding = fed.and_then(|e| e.search.as_ref()).is_some_and(|s| s.focused);
        match app.focus {
            Focus::Panes if finding => "Enter next · ⇧Enter previous · ⌘G/⌘⇧G next/previous · ⌥⌘C match case · Esc close",
            Focus::Panes if app.focused().and_then(|id| app.panes.get(&id)).is_some_and(|p| p.search.is_some()) => {
                "Enter search / open · ↑↓ select · ⌥⌘C case · ⌥⌘W word · ⌥⌘X regex · ⌘W close"
            }
            Focus::Panes if editing => "⌘F find · ⌘S save · ⌘Z undo · ⌘⇧Z redo · ⌘X/C line or selection · ⌘[ ] indent · ⌘W close · ⌘T terminal tab",
            Focus::Files => "Enter open · ←→ fold · right-click for actions · Esc back · ⌘⇧E hide",
            Focus::Sidebar => "Enter open · right-click for actions · a add project · w worktree · W workspace · x delete",
            Focus::Panes => "⌘P go to file · ⌘⇧F find in project · ⌘T tab · ⌘⇧[ ] tabs · ⌘D split · ⌘[ ] focus · ⌘K clear · ⌘E sidebar · ⌘B ⌘⇧A ⌘⇧E panels · ⌘↑↓ worktree · C-a prefix",
        }
    };
    let (mut fx, mut rx) = (x, r.w - x);
    for (panel, b) in app.panel_buttons() {
        if app.status_hover == Some(panel) {
            p.rect(b, HOVER);
        }
        let icon = match panel {
            Panel::Sidebar => &icons::PANEL_LEFT,
            Panel::Agents => &icons::BOT,
            Panel::Files => &icons::PANEL_RIGHT,
        };
        p.icon(icon, b, if app.panel_shown(panel) { TEXT } else { FAINT });
        if b.x > r.w / 2.0 {
            rx = rx.min(b.x - app.px(10.0));
        } else {
            fx = b.x + b.w + app.px(10.0);
        }
    }
    let rw = p.measure(right, 12.0, app.prefix);
    let lw = p.text(app.flash_text().unwrap_or_default(), R { x: fx, w: rx - rw - x - fx, ..r }, 12.0, MUTED, false);
    if rx > rw + lw + x + fx {
        p.text_right(right, rx, r, 12.0, if app.prefix { ACCENT } else { FAINT }, app.prefix);
    }
}
