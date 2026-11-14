//! Compose one frame from all panes + sidebar, then write only rows that changed.
use crate::app::{proj_key, ws_key, App, Row};
use crossterm::style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::{cursor, queue, terminal};
use std::io::{self, Write};

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const UNDER: u8 = 4;
const REV: u8 = 8;
const DIM: u8 = 16;

#[derive(Clone, Copy, PartialEq)]
pub struct Cell {
    ch: char,
    fg: Color,
    bg: Color,
    attr: u8,
    skip: bool,
}

const BLANK: Cell = Cell { ch: ' ', fg: Color::Reset, bg: Color::Reset, attr: 0, skip: false };

pub struct Frame {
    w: u16,
    h: u16,
    cells: Vec<Cell>,
}

impl Frame {
    fn new(w: u16, h: u16) -> Self {
        Frame { w, h, cells: vec![BLANK; w as usize * h as usize] }
    }
    fn set(&mut self, x: u16, y: u16, c: Cell) {
        if x < self.w && y < self.h {
            self.cells[y as usize * self.w as usize + x as usize] = c;
        }
    }
    /// Write text, clipped to `maxw`. Returns columns used.
    fn text(&mut self, x: u16, y: u16, maxw: u16, s: &str, fg: Color, bg: Color, attr: u8) -> u16 {
        let mut n = 0;
        for ch in s.chars() {
            if n >= maxw {
                break;
            }
            self.set(x + n, y, Cell { ch, fg, bg, attr, skip: false });
            n += 1;
        }
        n
    }
    fn fill(&mut self, x: u16, y: u16, w: u16, bg: Color) {
        for i in 0..w {
            self.set(x + i, y, Cell { bg, ..BLANK });
        }
    }
}

fn conv(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::AnsiValue(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb { r, g, b },
    }
}

fn glyph(state: Option<&str>) -> (char, Color) {
    match state {
        Some("working") => ('●', Color::AnsiValue(214)),
        Some("waiting") => ('!', Color::AnsiValue(203)),
        Some("idle") => ('○', Color::AnsiValue(114)),
        Some("done") => ('✓', Color::AnsiValue(114)),
        Some("error") => ('✗', Color::AnsiValue(203)),
        _ => ('•', Color::AnsiValue(242)),
    }
}

const GRAY: Color = Color::AnsiValue(242);
const SEL_BG: Color = Color::AnsiValue(24);
const BAR_BG: Color = Color::AnsiValue(235);

pub fn compose(app: &App) -> (Frame, Option<(u16, u16)>) {
    let mut f = Frame::new(app.w, app.h);
    let mut cursor = None;
    if app.show_sidebar {
        sidebar(app, &mut f);
    }
    let area = app.pane_area();
    let layout = app.layout();
    let focused = app.focused();
    if layout.is_empty() {
        let msg = if app.current.is_some() { "no panes: C-a | opens one" } else { "pick a worktree in the sidebar and press Enter" };
        let x = area.x + area.w.saturating_sub(msg.len() as u16) / 2;
        f.text(x, area.h / 2, area.w, msg, GRAY, Color::Reset, 0);
    }
    for (i, (id, r)) in layout.iter().enumerate() {
        let p = &app.panes[id];
        let is_focus = Some(*id) == focused && !app.sidebar_focus && app.prompt.is_none();
        // Header row.
        let (fg, bg) = if is_focus { (Color::White, SEL_BG) } else { (Color::AnsiValue(250), BAR_BG) };
        f.fill(r.x, r.y - 1, r.w, bg);
        let head = format!(" %{} {}", id, p.label());
        f.text(r.x, r.y - 1, r.w, &head, fg, bg, if is_focus { BOLD } else { 0 });
        if let Some(st) = p.state() {
            let (g, gc) = glyph(Some(st));
            let s = format!("{g} {st} ");
            let sw = s.chars().count() as u16;
            if r.w > sw + head.len() as u16 {
                f.text(r.x + r.w - sw, r.y - 1, sw, &s, gc, bg, 0);
            }
        }
        // Separator to the left of every pane but the first.
        if i > 0 {
            for y in r.y - 1..r.y + r.h {
                f.set(r.x - 1, y, Cell { ch: '│', fg: GRAY, ..BLANK });
            }
        }
        // Content.
        let s = p.parser.screen();
        for row in 0..r.h.min(p.rows) {
            for col in 0..r.w.min(p.cols) {
                let Some(c) = s.cell(row, col) else { continue };
                let mut attr = 0;
                if c.bold() { attr |= BOLD }
                if c.italic() { attr |= ITALIC }
                if c.underline() { attr |= UNDER }
                if c.inverse() { attr |= REV }
                if c.dim() { attr |= DIM }
                let ch = c.contents().chars().next().unwrap_or(' ');
                f.set(r.x + col, r.y + row, Cell { ch, fg: conv(c.fgcolor()), bg: conv(c.bgcolor()), attr, skip: c.is_wide_continuation() });
            }
        }
        if is_focus && !s.hide_cursor() {
            let (cr, cc) = s.cursor_position();
            cursor = Some((r.x + cc, r.y + cr));
        }
    }
    // Status bar.
    let y = app.h.saturating_sub(1);
    f.fill(0, y, app.w, BAR_BG);
    if let Some(p) = &app.prompt {
        let s = format!(" {}: {}", p.label, p.input);
        let n = f.text(0, y, app.w, &s, Color::White, BAR_BG, 0);
        cursor = Some((n, y));
    } else {
        let left = match &app.flash {
            Some((m, t)) if t.elapsed().as_secs() < 4 => format!(" {m}"),
            _ => match &app.current {
                Some(v) => {
                    let (ws, proj, wt) = app.names(v);
                    format!(" {ws} / {proj} / {wt}")
                }
                None => " pmux".into(),
            },
        };
        f.text(0, y, app.w, &left, Color::AnsiValue(250), BAR_BG, 0);
        let right = if app.prefix {
            "| pane  h/l focus  x close  c clear  r redraw  Tab sidebar  j/k worktree  n attention  b sidebar  q quit "
        } else if app.sidebar_focus {
            "Enter open  a project  w worktree  W workspace  D rm wt  x rm project  C-a q quit "
        } else {
            "C-a prefix "
        };
        let rw = right.chars().count() as u16;
        if app.w > rw + left.chars().count() as u16 {
            let (fg, at) = if app.prefix { (Color::AnsiValue(214), BOLD) } else { (GRAY, 0) };
            f.text(app.w - rw, y, rw, right, fg, BAR_BG, at);
        }
    }
    (f, cursor)
}

fn sidebar(app: &App, f: &mut Frame) {
    let w = app.sidebar_w;
    let h = app.h.saturating_sub(1);
    for y in 0..h {
        f.set(w, y, Cell { ch: '│', fg: GRAY, ..BLANK });
    }
    f.text(1, 0, w, "pmux", Color::AnsiValue(75), Color::Reset, BOLD);
    let rows = app.rows();
    let sel = app.sel.min(rows.len().saturating_sub(1));
    let vis = h.saturating_sub(2) as usize;
    let top = app.sidebar_top(rows.len());
    for (i, row) in rows.iter().enumerate().skip(top).take(vis) {
        let y = (i - top) as u16 + 2;
        let selected = app.sidebar_focus && i == sel;
        let view = app.row_view(*row);
        let current = view.is_some() && view == app.current;
        let bg = if selected { SEL_BG } else if app.hover == Some(i) { Color::AnsiValue(237) } else { Color::Reset };
        f.fill(0, y, w, bg);
        let (indent, label, arrow, attr, fg, panes): (u16, String, &str, u8, Color, Vec<u32>) = match *row {
            Row::Ws(wi) => {
                let ws = &app.workspaces[wi];
                let folded = app.folded.contains(&ws_key(&ws.name));
                // Folded: roll up every pane under it.
                let panes = if folded { app.panes.values().filter(|p| app.names(&p.view).0 == ws.name).map(|p| p.id).collect() } else { vec![] };
                (1, ws.name.clone(), if folded { "▸ " } else { "▾ " }, BOLD, Color::White, panes)
            }
            Row::Proj(wi, pi) => {
                let p = &app.workspaces[wi].projects[pi];
                let folded = app.folded.contains(&proj_key(&p.path));
                let arrow = if !p.has_linked() { "  " } else if folded { "▸ " } else { "▾ " };
                // Main checkout panes; folded also rolls up the linked worktrees.
                let main_listed = p.listed().any(|(_, w)| w.main);
                let panes: Vec<u32> = if folded {
                    p.view_paths().iter().flat_map(|v| app.panes_in(v).to_vec()).collect()
                } else if main_listed {
                    vec![]
                } else {
                    app.panes_in(&p.path).to_vec()
                };
                (3, p.name.clone(), arrow, 0, Color::AnsiValue(252), panes)
            }
            Row::Wt(wi, pi, ti) => {
                let t = &app.workspaces[wi].projects[pi].worktrees.as_ref().unwrap()[ti];
                let fg = if app.panes_in(&t.path).is_empty() { GRAY } else { Color::AnsiValue(252) };
                (5, t.branch.clone(), "", 0, fg, app.panes_in(&t.path).to_vec())
            }
        };
        let (fg, attr) = if current { (Color::AnsiValue(117), attr | BOLD) } else { (fg, attr) };
        let mut x = indent;
        if current {
            f.text(0, y, 1, "▌", Color::AnsiValue(117), bg, 0);
        }
        x += f.text(x, y, w - x, arrow, GRAY, bg, 0);
        let gw = (panes.len() as u16).min(6);
        x += f.text(x, y, w.saturating_sub(x + gw + 1), &label, fg, bg, attr);
        x += 1;
        let mut ps: Vec<_> = panes.iter().map(|id| &app.panes[id]).collect();
        ps.sort_by_key(|p| p.id);
        for p in ps.iter().take(6) {
            let (g, gc) = glyph(p.state());
            x += f.text(x, y, 1, &g.to_string(), gc, bg, 0);
        }
    }
}

#[derive(Default)]
pub struct Renderer {
    prev: Option<Frame>,
    pointer: &'static str,
}

impl Renderer {
    /// Forget what's on screen, so the next draw repaints everything.
    pub fn invalidate(&mut self) {
        self.prev = None;
    }

    pub fn draw(&mut self, out: &mut impl Write, f: Frame, cursor: Option<(u16, u16)>, pointer: &'static str) -> io::Result<()> {
        if pointer != self.pointer {
            // OSC 22: mouse pointer shape (Ghostty, kitty, WezTerm, foot, xterm). Others ignore it.
            write!(out, "\x1b]22;{pointer}\x1b\\")?;
            self.pointer = pointer;
        }
        let full = self.prev.as_ref().is_none_or(|p| p.w != f.w || p.h != f.h);
        let w = f.w as usize;
        let changed: Vec<usize> = (0..f.h as usize)
            .filter(|&y| full || self.prev.as_ref().unwrap().cells[y * w..(y + 1) * w] != f.cells[y * w..(y + 1) * w])
            .collect();
        queue!(out, terminal::BeginSynchronizedUpdate, cursor::Hide)?;
        if full {
            queue!(out, terminal::Clear(terminal::ClearType::All))?;
        }
        for y in changed {
            queue!(out, cursor::MoveTo(0, y as u16))?;
            let mut style = None;
            for c in &f.cells[y * w..(y + 1) * w] {
                if c.skip {
                    continue;
                }
                if style != Some((c.fg, c.bg, c.attr)) {
                    queue!(out, SetAttribute(Attribute::Reset), SetForegroundColor(c.fg), SetBackgroundColor(c.bg))?;
                    for (bit, a) in [(BOLD, Attribute::Bold), (ITALIC, Attribute::Italic), (UNDER, Attribute::Underlined), (REV, Attribute::Reverse), (DIM, Attribute::Dim)] {
                        if c.attr & bit != 0 {
                            queue!(out, SetAttribute(a))?;
                        }
                    }
                    style = Some((c.fg, c.bg, c.attr));
                }
                queue!(out, Print(c.ch))?;
            }
        }
        queue!(out, SetAttribute(Attribute::Reset))?;
        if let Some((x, y)) = cursor {
            queue!(out, cursor::MoveTo(x, y), cursor::Show)?;
        }
        queue!(out, terminal::EndSynchronizedUpdate)?;
        out.flush()?;
        self.prev = Some(f);
        Ok(())
    }
}
