//! One terminal's emulator state: alacritty_terminal's grid + parser, fed on the main thread.
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use std::cell::RefCell;
use std::rc::Rc;

/// Collects terminal events (replies to queries, title changes, bell) for the app to handle after a feed.
#[derive(Clone, Default)]
pub struct Listener(Rc<RefCell<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, e: Event) {
        self.0.borrow_mut().push(e);
    }
}

#[derive(Clone, Copy)]
pub struct Size {
    pub cols: usize,
    pub lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// A terminal emulator: grid + scrollback (10k lines) and the escape-sequence parser.
/// Events the terminal raises while parsing (replies, title, bell) come back from `feed`.
pub struct Vt {
    pub term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    events: Listener,
}

impl Vt {
    pub fn new(cols: usize, lines: usize) -> Self {
        let events = Listener::default();
        let cfg = Config { scrolling_history: 10_000, ..Default::default() };
        let term = Term::new(cfg, &Size { cols: cols.max(2), lines: lines.max(1) }, events.clone());
        Vt { term, parser: Processor::new(), events }
    }

    /// Parse output from the child. Returns events the app must act on.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.parser.advance(&mut self.term, bytes);
        std::mem::take(&mut *self.events.0.borrow_mut())
    }

    pub fn resize(&mut self, cols: usize, lines: usize) {
        self.term.resize(Size { cols: cols.max(2), lines: lines.max(1) });
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn scroll(&mut self, s: Scroll) {
        self.term.scroll_display(s);
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Visible screen (bottom of scrollback) as plain text.
    pub fn text(&self) -> String {
        let g = self.term.grid();
        let mut out = String::new();
        for l in 0..g.screen_lines() {
            let row = &g[Line(l as i32)];
            let mut line = String::new();
            for c in 0..g.columns() {
                let cell = &row[Column(c)];
                if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    line.push(cell.c);
                }
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    /// Clear screen and scrollback (Cmd+K).
    pub fn clear(&mut self) {
        self.feed(b"\x1b[H\x1b[2J\x1b[3J");
    }
}

use alacritty_terminal::vte::ansi::{Color, NamedColor};
use std::fmt::Write as _;

/// SGR parameters for a colour: `base` 30 for foreground, 40 for background.
fn sgr_color(out: &mut String, c: Color, base: u8) {
    match c {
        Color::Named(n) => {
            let i = n as usize;
            match i {
                0..=7 => write!(out, ";{}", base as usize + i),
                8..=15 => write!(out, ";{}", base as usize + 60 + i - 8),
                _ => match n {
                    NamedColor::DimBlack
                    | NamedColor::DimRed
                    | NamedColor::DimGreen
                    | NamedColor::DimYellow
                    | NamedColor::DimBlue
                    | NamedColor::DimMagenta
                    | NamedColor::DimCyan
                    | NamedColor::DimWhite => write!(out, ";{}", base as usize + (i - NamedColor::DimBlack as usize)),
                    _ => Ok(()), // default fg/bg: SGR 0 already reset it
                },
            }
        }
        Color::Indexed(i) => write!(out, ";{};5;{i}", base + 8),
        Color::Spec(r) => write!(out, ";{};2;{};{};{}", base + 8, r.r, r.g, r.b),
    }
    .ok();
}

/// The SGR sequence that sets a cell's look from scratch.
fn sgr(c: &alacritty_terminal::term::cell::Cell) -> String {
    let mut s = String::from("\x1b[0");
    let f = c.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if f.contains(flag) {
            s.push(';');
            s.push_str(code);
        }
    }
    if f.contains(Flags::DOUBLE_UNDERLINE) {
        s.push_str(";21");
    } else if f.contains(Flags::UNDERCURL) {
        s.push_str(";4:3");
    } else if f.contains(Flags::DOTTED_UNDERLINE) {
        s.push_str(";4:4");
    } else if f.contains(Flags::DASHED_UNDERLINE) {
        s.push_str(";4:5");
    } else if f.contains(Flags::UNDERLINE) {
        s.push_str(";4");
    }
    sgr_color(&mut s, c.fg, 30);
    sgr_color(&mut s, c.bg, 40);
    s.push('m');
    s
}

impl Vt {
    /// The whole terminal (scrollback, screen, cursor, modes) as bytes that rebuild it when fed
    /// to a fresh `Vt` of the same size. Used to hand a running terminal to a newly opened window.
    pub fn dump(&self, title: Option<&str>) -> Vec<u8> {
        let g = self.term.grid();
        let mode = self.mode();
        let alt = mode.contains(TermMode::ALT_SCREEN);
        let mut out = String::new();
        if alt {
            out.push_str("\x1b[?1049h\x1b[H");
        }
        let (cols, screen) = (g.columns(), g.screen_lines() as i32);
        let top = if alt { 0 } else { -(g.history_size() as i32) };
        let mut look = String::new();
        for l in top..screen {
            let row = &g[Line(l)];
            let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            // Trailing blank cells add nothing (unless the line wraps: then they're part of it).
            let plain = |c: &alacritty_terminal::term::cell::Cell| {
                c.c == ' ' && c.bg == Color::Named(NamedColor::Background) && c.flags.difference(Flags::WRAPLINE).is_empty()
            };
            let end = if wrapped { cols } else { (0..cols).rev().find(|&c| !plain(&row[Column(c)])).map_or(0, |c| c + 1) };
            for c in 0..end {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let s = sgr(cell);
                if s != look {
                    out.push_str(&s);
                    look = s;
                }
                out.push(cell.c);
                for z in cell.zerowidth().unwrap_or(&[]) {
                    out.push(*z);
                }
            }
            if l + 1 < screen && !wrapped {
                if look != "\x1b[0m" {
                    out.push_str("\x1b[0m");
                    look = "\x1b[0m".into();
                }
                out.push_str("\r\n");
            }
        }
        let cur = g.cursor.point;
        let _ = write!(out, "\x1b[0m\x1b[{};{}H", cur.line.0 + 1, cur.column.0 + 1);
        out.push_str(&sgr(&g.cursor.template));
        for (m, n) in [
            (TermMode::APP_CURSOR, 1),
            (TermMode::MOUSE_REPORT_CLICK, 1000),
            (TermMode::MOUSE_DRAG, 1002),
            (TermMode::MOUSE_MOTION, 1003),
            (TermMode::FOCUS_IN_OUT, 1004),
            (TermMode::SGR_MOUSE, 1006),
            (TermMode::ALTERNATE_SCROLL, 1007),
            (TermMode::BRACKETED_PASTE, 2004),
        ] {
            if mode.contains(m) {
                let _ = write!(out, "\x1b[?{n}h");
            }
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.push_str("\x1b[?25l");
        }
        if !mode.contains(TermMode::LINE_WRAP) {
            out.push_str("\x1b[?7l");
        }
        if mode.contains(TermMode::APP_KEYPAD) {
            out.push_str("\x1b=");
        }
        let kitty = (mode & TermMode::KITTY_KEYBOARD_PROTOCOL).bits() >> 18;
        if kitty != 0 {
            let _ = write!(out, "\x1b[>{kitty}u");
        }
        if let Some(t) = title {
            let _ = write!(out, "\x1b]2;{t}\x07");
        }
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::Vt;
    use alacritty_terminal::grid::Dimensions;

    #[test]
    fn dump_rebuilds_screen() {
        let mut a = Vt::new(20, 4);
        a.feed(b"hello \x1b[31mred\x1b[0m\r\nline2\r\n");
        for i in 0..10 {
            a.feed(format!("scroll {i}\r\n").as_bytes());
        }
        a.feed(b"\x1b[?2004h\x1b[1;5Hx");
        let mut b = Vt::new(20, 4);
        b.feed(&a.dump(None));
        assert_eq!(a.text(), b.text());
        assert_eq!(a.term.grid().history_size(), b.term.grid().history_size());
        assert_eq!(a.term.grid().cursor.point, b.term.grid().cursor.point);
        assert_eq!(a.mode(), b.mode());
    }

    #[test]
    fn dump_keeps_wrapped_lines() {
        let mut a = Vt::new(5, 3);
        a.feed(b"abcdefgh");
        let mut b = Vt::new(5, 3);
        b.feed(&a.dump(None));
        assert_eq!(a.text(), b.text());
        b.resize(10, 3);
        assert!(b.text().starts_with("abcdefgh"));
    }
}
