//! Build one frame's draw list: sidebar, pane headers, terminal grids, status bar.
use crate::app::{proj_key, ws_key, App, Row, R, ROW_H, ROW_INSET, SB_TOP};
use crate::gpu::{Inst, COLOR, MASK, RECT};
use crate::text::Fonts;
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
const BORDER: C = rgb(0x2c, 0x2c, 0x32);
const TEXT: C = rgb(0xe4, 0xe4, 0xe7);
const MUTED: C = rgb(0x8b, 0x8b, 0x94);
const FAINT: C = rgb(0x5c, 0x5c, 0x66);
const ACCENT: C = rgb(0x5b, 0x9b, 0xff);
const HOVER: C = rgb(0x22, 0x22, 0x27);
const SELECT: C = rgb(0x26, 0x33, 0x4a);
const MENU_BG: C = rgb(0x20, 0x20, 0x25);
const DANGER: C = rgb(0xf0, 0x6a, 0x6a);
/// ANSI red, as herdr colors its behind count.
const BEHIND: C = rgb(0xe0, 0x6c, 0x75);
const GUIDE: C = rgb(0x2e, 0x2e, 0x35);
const GUIDE_ACTIVE: C = rgb(0x52, 0x52, 0x5e);
/// ACCENT at 12% (premultiplied alpha).
const DRAG_TINT: C = [0x5b as f32 / 255.0 * 0.12, 0x9b as f32 / 255.0 * 0.12, 0.12, 0.12];
const CURSOR: C = rgb(0xd8, 0xd8, 0xd8);

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

fn state_color(s: Option<&str>) -> C {
    match s {
        Some("working") => rgb(0xf5, 0xa5, 0x24),
        Some("waiting") | Some("error") => rgb(0xef, 0x5b, 0x5b),
        Some("idle") | Some("done") => rgb(0x4c, 0xc2, 0x6a),
        _ => FAINT,
    }
}

pub struct Painter<'a> {
    pub out: Vec<Inst>,
    pub fonts: &'a mut Fonts,
}

impl Painter<'_> {
    pub fn rect(&mut self, r: R, color: C, radius: f32) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        self.out.push(Inst { rect: [r.x, r.y, r.w, r.h], uv: [0.0; 4], color, misc: [RECT, radius, 0.0, 0.0] });
    }

    fn glyph(&mut self, g: crate::text::Glyph, x: f32, y: f32, color: C) {
        let rect = [x + g.left as f32, y - g.top as f32, g.uv[2], g.uv[3]];
        self.out.push(Inst { rect, uv: g.uv, color, misc: [if g.color { COLOR } else { MASK }, 0.0, 0.0, 0.0] });
    }

    /// Draw text vertically centered in `[y, y+h)`, clipped with an ellipsis at `max_w`. Returns width drawn.
    pub fn text(&mut self, s: &str, x: f32, y: f32, h: f32, pt: f32, color: C, bold: bool, max_w: f32) -> f32 {
        if max_w <= 0.0 || s.is_empty() {
            return 0.0;
        }
        let sh = self.fonts.ui(s, pt, bold, false);
        let lh = self.fonts.line_h(pt);
        let base = (y + ((h - lh) / 2.0).round() + sh.ascent.round()).round();
        let x = x.round();
        let clip = sh.width > max_w;
        let ell = if clip { self.fonts.ui("…", pt, bold, false) } else { sh.clone() };
        let limit = if clip { max_w - ell.width } else { f32::MAX };
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
        if clip {
            for &(key, gx, gy, _) in &ell.glyphs {
                if let Some(g) = self.fonts.glyph(key) {
                    self.glyph(g, x + end + gx as f32, base + gy as f32, color);
                }
            }
            return end + ell.width;
        }
        sh.width
    }

    pub fn measure(&mut self, s: &str, pt: f32, bold: bool) -> f32 {
        self.fonts.ui(s, pt, bold, false).width
    }

    /// Draw an icon mask from its top-left corner.
    fn icon(&mut self, g: crate::text::Glyph, x: f32, y: f32, color: C) {
        self.out.push(Inst { rect: [x.round(), y.round(), g.uv[2], g.uv[3]], uv: g.uv, color, misc: [MASK, 0.0, 0.0, 0.0] });
    }

    fn dot(&mut self, cx: f32, cy: f32, d: f32, color: C) {
        self.rect(R { x: (cx - d / 2.0).round(), y: (cy - d / 2.0).round(), w: d, h: d }, color, d / 2.0);
    }
}

pub fn frame(app: &App, fonts: &mut Fonts) -> Vec<Inst> {
    let mut p = Painter { out: Vec::with_capacity(8192), fonts };
    if app.show_sidebar {
        sidebar(app, &mut p);
    }
    panes(app, &mut p);
    status(app, &mut p);
    menu(app, &mut p);
    p.out
}

/// Sidebar laid out like Zed's project panel: chevrons, folder/branch icons, indent guide lines.
fn sidebar(app: &App, p: &mut Painter) {
    use crate::text::{ICON_BRANCH, ICON_FOLDER, ICON_FOLDER_OPEN};
    let s = app.scale;
    let sb = app.sidebar_rect();
    p.rect(sb, SIDEBAR, 0.0);
    p.rect(R { x: sb.w - s.floor().max(1.0), w: s.floor().max(1.0), ..sb }, BORDER, 0.0);

    let rows = app.rows();
    let sel = app.sel.min(rows.len().saturating_sub(1));
    let top = app.sidebar_top(rows.len());
    let vis = app.sidebar_visible_rows();
    let rh = app.px(ROW_H);
    let inset = app.px(ROW_INSET);
    let row_y = |i: usize| app.px(SB_TOP) + (i as f32 - top as f32) * rh;
    let current_row = rows.iter().position(|r| app.row_view(*r).is_some() && app.row_view(*r) == app.current);

    // Row backgrounds.
    for i in top..(top + vis).min(rows.len()) {
        let r = R { x: inset, y: row_y(i), w: sb.w - 2.0 * inset, h: rh };
        let bg = if app.sidebar_focus && i == sel {
            Some(SELECT)
        } else if Some(i) == current_row {
            Some(HEADER_FOCUS)
        } else if app.hover == Some(i) {
            Some(HOVER)
        } else {
            None
        };
        if let Some(c) = bg {
            p.rect(r, c, app.px(4.0));
        }
    }

    // Indent guides: one line under each open parent's chevron, spanning its children.
    let (clip_top, clip_bot) = (app.px(SB_TOP), app.sidebar_rect().h);
    let line = s.floor().max(1.0);
    for (i, row) in rows.iter().enumerate() {
        let lv = App::level(*row);
        let end = rows[i + 1..].iter().position(|r| App::level(*r) <= lv).map_or(rows.len(), |k| i + 1 + k);
        if end == i + 1 {
            continue;
        }
        let active = current_row.is_some_and(|c| c > i && c < end);
        let cr = app.chevron_rect(lv, 0.0);
        let x = (cr.x + cr.w / 2.0).floor();
        let y0 = (row_y(i + 1) + app.px(2.0)).max(clip_top);
        let y1 = (row_y(end) - app.px(2.0)).min(clip_bot);
        if y1 > y0 {
            p.rect(R { x, y: y0, w: line, h: y1 - y0 }, if active { GUIDE_ACTIVE } else { GUIDE }, 0.0);
        }
    }

    for (i, row) in rows.iter().enumerate().skip(top).take(vis) {
        let y = row_y(i);
        let r = R { x: inset, y, w: sb.w - 2.0 * inset, h: rh };
        let current = Some(i) == current_row;
        let lv = App::level(*row);
        // (label, icon, fg, bold, panes rolled into this row). Folders show open/closed instead of a chevron.
        let mut behind_of: Option<&std::path::Path> = None;
        let (label, icon, mut fg, bold, panes): (String, &'static crate::text::Icon, C, bool, Vec<u32>) = match *row {
            Row::Ws(wi) => {
                let ws = &app.workspaces[wi];
                let folded = app.folded.contains(&ws_key(&ws.name));
                let panes = if folded { app.panes.values().filter(|pn| app.names(&pn.view).0 == ws.name).map(|pn| pn.id).collect() } else { vec![] };
                let open = !folded && !ws.projects.is_empty();
                (ws.name.clone(), if open { &ICON_FOLDER_OPEN } else { &ICON_FOLDER }, TEXT, true, panes)
            }
            Row::Proj(wi, pi) => {
                let pr = &app.workspaces[wi].projects[pi];
                let folded = app.folded.contains(&proj_key(&pr.path));
                let main_listed = pr.listed().any(|(_, w)| w.main);
                let panes: Vec<u32> = if folded {
                    pr.view_paths().iter().flat_map(|v| app.panes_in(v).to_vec()).collect()
                } else if main_listed {
                    vec![]
                } else {
                    app.panes_in(&pr.path).to_vec()
                };
                let open = pr.has_linked() && !folded;
                if !main_listed {
                    behind_of = Some(&pr.path);
                }
                (pr.name.clone(), if open { &ICON_FOLDER_OPEN } else { &ICON_FOLDER }, TEXT, false, panes)
            }
            Row::Wt(wi, pi, ti) => {
                let t = &app.workspaces[wi].projects[pi].worktrees.as_ref().unwrap()[ti];
                let fg = if app.panes_in(&t.path).is_empty() { MUTED } else { TEXT };
                behind_of = Some(&t.path);
                (t.branch.clone(), &ICON_BRANCH, fg, false, app.panes_in(&t.path).to_vec())
            }
        };
        if current {
            fg = ACCENT;
        }
        let cr = app.chevron_rect(lv, y);
        let icon_sz = 14.0;
        let d = app.px(icon_sz);
        let mid = |h: f32| y + ((rh - h) / 2.0).round();
        // Icon sits in the level's box, centered over the guide line of its children.
        if let Some(g) = p.fonts.icon(icon, icon_sz) {
            p.icon(g, cr.x + ((cr.w - d) / 2.0).round(), mid(d), if current { ACCENT } else { MUTED });
        }
        let x = cr.x + cr.w + app.px(6.0);
        // State dots on the right.
        let mut ps: Vec<_> = panes.iter().map(|id| &app.panes[id]).collect();
        ps.sort_by_key(|pn| pn.id);
        let n = ps.len().min(6);
        let dd = app.px(7.0);
        let gap = app.px(5.0);
        let dots_w = n as f32 * (dd + gap);
        let right = r.x + r.w - app.px(8.0);
        for (k, pn) in ps.iter().take(6).enumerate() {
            let cx = right - dots_w + k as f32 * (dd + gap) + dd / 2.0 + gap;
            p.dot(cx, y + rh / 2.0, dd, state_color(pn.state()));
        }
        let mut label_end = right - dots_w - app.px(4.0);
        // Behind upstream: red ↓N, left of the dots.
        if let Some(n) = behind_of.and_then(|v| app.behind.get(v)) {
            let t = format!("\u{2193}{n}");
            let w = p.measure(&t, 11.5, false);
            let bx = label_end - w;
            p.text(&t, bx, y, rh, 11.5, BEHIND, false, w + 1.0);
            label_end = bx - app.px(6.0);
        }
        // Hover on a workspace: a + (add project) replaces the state dots. Everything else is in the right-click menu.
        if app.hover == Some(i) && matches!(row, Row::Ws(_)) && app.menu.is_none() {
            let row_bg = if app.sidebar_focus && i == sel { SELECT } else if current { HEADER_FOCUS } else { HOVER };
            let btns = vec![(app.plus_button(y), "+", 2u8)];
            let left = btns.last().unwrap().0.x;
            p.rect(R { x: left - app.px(6.0), w: r.x + r.w - left + app.px(6.0), ..r }, row_bg, app.px(4.0));
            for (b, g, id) in btns {
                let on = app.hover_btn == id;
                if on {
                    p.rect(b, BORDER, app.px(4.0));
                }
                let w = p.measure(g, 14.0, false);
                p.text(g, b.x + (b.w - w) / 2.0, b.y, b.h, 14.0, if on { TEXT } else { MUTED }, false, b.w);
            }
            label_end = left - app.px(6.0);
        }
        p.text(&label, x, y, rh, 13.0, fg, bold || current, label_end - x);
    }

    // Reorder drag: the dragged row stays highlighted; a line marks where it lands.
    if let Some(d) = app.drag.as_ref().filter(|d| d.active) {
        if let Some(i) = rows.iter().position(|r| *r == d.row).filter(|i| (top..top + vis).contains(i)) {
            p.rect(R { x: inset, y: row_y(i), w: sb.w - 2.0 * inset, h: rh }, DRAG_TINT, app.px(4.0));
        }
        if let Some((_, b, lv)) = d.target {
            let th = app.px(2.0);
            let y = (row_y(b) - th / 2.0).clamp(clip_top, clip_bot - th);
            let x0 = app.chevron_rect(lv, 0.0).x;
            p.rect(R { x: x0, y, w: sb.w - inset - x0, h: th }, ACCENT, th / 2.0);
        }
    }
}

fn panes(app: &App, p: &mut Painter) {
    let area = app.pane_area();
    let layout = app.layout();
    let focused = app.focused();
    if layout.is_empty() {
        let msg = if app.current.is_some() { "No panes. Press ⌘D to open one." } else { "Pick a worktree in the sidebar." };
        let w = p.measure(msg, 13.0, false);
        p.text(msg, area.x + (area.w - w) / 2.0, area.y + area.h / 2.0 - app.px(10.0), app.px(20.0), 13.0, MUTED, false, area.w);
        return;
    }
    let sep = app.scale.floor().max(1.0);
    for (i, pr) in layout.iter().enumerate() {
        let pane = &app.panes[&pr.id];
        let is_focus = Some(pr.id) == focused && !app.sidebar_focus && app.prompt.is_none();
        if i > 0 {
            p.rect(R { x: pr.header.x - sep, y: area.y, w: sep, h: area.h }, BORDER, 0.0);
        }
        // No header: with several panes, a thin accent line marks the focused one.
        if is_focus && layout.len() > 1 {
            p.rect(R { h: app.px(2.0), ..pr.header }, ACCENT, 0.0);
        }
        // Scrolled back: small badge in the top-right corner.
        let off = pane.vt.display_offset();
        if off > 0 {
            let s = format!("↑ {off}");
            let w = p.measure(&s, 11.0, false) + app.px(14.0);
            let b = R { x: pr.body.x + pr.body.w - w, y: pr.body.y, w, h: app.px(20.0) };
            p.rect(b, SELECT, app.px(10.0));
            p.text(&s, b.x + app.px(7.0), b.y, b.h, 11.0, TEXT, false, w);
        }
        grid(app, p, pane, pr.body, is_focus);
    }
}

fn resolve(term: &alacritty_terminal::Term<crate::term::Listener>, c: Color) -> C {
    match c {
        Color::Spec(rgb) => to_c(rgb),
        Color::Indexed(i) => to_c(term.colors()[i as usize].unwrap_or_else(|| default_color(i as usize))),
        Color::Named(n) => to_c(term.colors()[n as usize].unwrap_or_else(|| default_color(n as usize))),
    }
}

fn grid(app: &App, p: &mut Painter, pane: &crate::app::Pane, body: R, focus: bool) {
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
    let mut glyphs: Vec<(char, u8, f32, f32, C)> = Vec::with_capacity(lines * cols);
    for l in 0..lines {
        let row = &g[Line(l as i32 - off)];
        let y = body.y + l as f32 * ch;
        // Background runs.
        let mut run: Option<(usize, C)> = None;
        for c in 0..=cols {
            let bg = (c < cols).then(|| {
                let cell = &row[Column(c)];
                let (fg, bg) = (cell.fg, cell.bg);
                if cell.flags.contains(Flags::INVERSE) { resolve(term, fg) } else { resolve(term, bg) }
            });
            let same = matches!((&run, &bg), (Some((_, a)), Some(b)) if a == b);
            if !same {
                if let Some((start, col)) = run.take() {
                    if col != default_bg {
                        p.rect(R { x: body.x + start as f32 * cw, y, w: (c - start) as f32 * cw, h: ch }, col, 0.0);
                    }
                }
                run = bg.map(|b| (c, b));
            }
        }
        for c in 0..cols {
            let cell = &row[Column(c)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN) {
                continue;
            }
            let mut fg = if cell.flags.contains(Flags::INVERSE) { resolve(term, cell.bg) } else { resolve(term, cell.fg) };
            if cell.flags.contains(Flags::DIM) {
                fg = [fg[0] * 0.66, fg[1] * 0.66, fg[2] * 0.66, 1.0];
            }
            let x = body.x + c as f32 * cw;
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                p.rect(R { x, y: y + base + app.px(2.0), w: cw, h: app.scale.max(1.0) }, fg, 0.0);
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                p.rect(R { x, y: y + (ch / 2.0).round(), w: cw, h: app.scale.max(1.0) }, fg, 0.0);
            }
            if cell.c != ' ' && cell.c != '\t' {
                let style = cell.flags.contains(Flags::BOLD) as u8 | (cell.flags.contains(Flags::ITALIC) as u8) << 1;
                glyphs.push((cell.c, style, x, y + base, fg));
            }
        }
    }
    // Cursor sits between backgrounds and text.
    let cur_l = cur.line.0;
    let mut cursor_cell = None;
    if show_cursor && cur_l >= 0 && (cur_l as usize) < lines && cur.column.0 < cols {
        let r = R { x: body.x + cur.column.0 as f32 * cw, y: body.y + cur_l as f32 * ch, w: cw, h: ch };
        if focus {
            p.rect(r, CURSOR, 0.0);
            cursor_cell = Some((r.x, r.y + base));
        } else {
            let t = app.scale.max(1.0);
            p.rect(R { h: t, ..r }, FAINT, 0.0);
            p.rect(R { y: r.y + r.h - t, h: t, ..r }, FAINT, 0.0);
            p.rect(R { w: t, ..r }, FAINT, 0.0);
            p.rect(R { x: r.x + r.w - t, w: t, ..r }, FAINT, 0.0);
        }
    }
    for (c, style, x, y, mut fg) in glyphs {
        if cursor_cell == Some((x, y)) {
            fg = BG;
        }
        if let Some((gl, gx, gy)) = p.fonts.cell(c, style) {
            p.glyph(gl, x + gx as f32, y + gy as f32, fg);
        }
    }
}

/// Right-click context menu, drawn on top of everything.
fn menu(app: &App, p: &mut Painter) {
    let Some(m) = &app.menu else { return };
    let (w, h) = app.menu_size(&m.items);
    let r = R { x: m.x, y: m.y, w, h };
    // Soft shadow, border, body.
    p.rect(R { x: r.x - app.px(1.0), y: r.y + app.px(2.0), w: r.w + app.px(2.0), h: r.h + app.px(2.0) }, [0.0, 0.0, 0.0, 0.35], app.px(8.0));
    p.rect(r, BORDER, app.px(7.0));
    let b = app.scale.floor().max(1.0);
    p.rect(R { x: r.x + b, y: r.y + b, w: r.w - 2.0 * b, h: r.h - 2.0 * b }, MENU_BG, app.px(6.0));
    for (i, (ir, (label, a))) in app.menu_rects().into_iter().zip(&m.items).enumerate() {
        if matches!(a, crate::app::Action::Sep) {
            p.rect(R { x: ir.x + app.px(4.0), y: ir.y + (ir.h / 2.0).floor(), w: ir.w - app.px(8.0), h: b }, BORDER, 0.0);
            continue;
        }
        if m.hover == Some(i) {
            p.rect(ir, SELECT, app.px(4.0));
        }
        let danger = matches!(a, crate::app::Action::Delete(_));
        p.text(label, ir.x + app.px(10.0), ir.y, ir.h, 13.0, if danger { DANGER } else { TEXT }, false, ir.w - app.px(20.0));
    }
}

fn status(app: &App, p: &mut Painter) {
    let r = app.status_rect();
    p.rect(r, SIDEBAR, 0.0);
    p.rect(R { h: app.scale.floor().max(1.0), ..r }, BORDER, 0.0);
    let x = app.px(14.0);
    if let Some(pr) = &app.prompt {
        let lw = p.text(&format!("{}:", pr.label), x, r.y, r.h, 12.0, ACCENT, true, r.w);
        let iw = p.text(&pr.input, x + lw + app.px(8.0), r.y, r.h, 12.0, TEXT, false, r.w);
        let cx = x + lw + app.px(8.0) + iw + app.px(1.0);
        p.rect(R { x: cx, y: r.y + app.px(6.0), w: app.px(1.5), h: r.h - app.px(12.0) }, TEXT, 0.0);
        let hint = "Enter confirm · Esc cancel · Tab complete";
        let w = p.measure(hint, 12.0, false);
        p.text(hint, r.w - w - x, r.y, r.h, 12.0, FAINT, false, w + 1.0);
        return;
    }
    let left = match &app.flash {
        Some((m, t)) if t.elapsed().as_secs() < 4 => m.clone(),
        _ => String::new(),
    };
    let right = if app.prefix {
        "| split · h/l focus · x close · c clear · j/k worktree · n attention · Tab sidebar · b hide · q quit"
    } else if app.sidebar_focus {
        "Enter open · right-click for actions · a add project · w worktree · W workspace · D delete worktree · x delete"
    } else {
        "⌘D split · ⌘[ ] focus · ⌘K clear · ⌘E sidebar · ⌘↑↓ worktree · C-a prefix"
    };
    let rw = p.measure(right, 12.0, false);
    let lw = p.text(&left, x, r.y, r.h, 12.0, MUTED, false, r.w - rw - 3.0 * x);
    if r.w > rw + lw + 3.0 * x {
        p.text(right, r.w - rw - x, r.y, r.h, 12.0, if app.prefix { ACCENT } else { FAINT }, app.prefix, rw + 1.0);
    }
}
