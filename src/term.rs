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
