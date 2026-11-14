//! A text buffer for editor panes: lines, a cursor with an optional selection, undo/redo, save.
//! Positions are (line, column) with the column counted in chars. No rendering or input here.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Display width of a tab, in cells.
pub const TAB: usize = 4;
/// Typing within this long of the last edit joins its undo step.
const UNDO_JOIN: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Pos { line, col }
    }
}

#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cur: Pos,
}

/// What kind of edit last happened, to group typing into one undo step.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Other,
}

pub struct Editor {
    pub path: PathBuf,
    pub lines: Vec<String>,
    pub cur: Pos,
    /// Selection start; the selection runs between `anchor` and `cur`.
    pub anchor: Option<Pos>,
    /// Column the cursor tries to keep on up/down moves (display cells).
    want_x: Option<usize>,
    /// First visible line and display column.
    pub top: usize,
    pub left: usize,
    crlf: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<(EditKind, Instant)>,
    /// Lines as last saved, to tell whether the buffer is modified.
    saved: Vec<String>,
    /// ⌘W pressed once on a modified buffer: the next one discards.
    pub close_armed: bool,
    /// Tree-sitter language, if we have one for this file.
    pub lang: Option<&'static str>,
    /// Highlight spans per line, as of `hl_version`.
    pub hl: Vec<Vec<crate::syntax::Span>>,
    /// Bumped on every text change.
    version: u64,
    hl_version: u64,
}

fn byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

fn len(s: &str) -> usize {
    s.chars().count()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Char class for word motion: 0 space, 1 word, 2 punctuation.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if is_word(c) {
        1
    } else {
        2
    }
}

impl Editor {
    pub fn open(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if bytes.contains(&0) {
            return Err("binary file".into());
        }
        let text = String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())?;
        let mut e = Self::from_text(path.to_path_buf(), &text);
        e.refresh_highlight();
        Ok(e)
    }

    pub fn from_text(path: PathBuf, text: &str) -> Self {
        let crlf = text.contains("\r\n");
        let lines: Vec<String> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
        let lang = crate::syntax::lang_for(&path);
        Editor {
            path,
            saved: lines.clone(),
            lines,
            cur: Pos::default(),
            anchor: None,
            want_x: None,
            top: 0,
            left: 0,
            crlf,
            undo: vec![],
            redo: vec![],
            last_edit: None,
            close_armed: false,
            lang,
            hl: vec![],
            version: 1,
            hl_version: 0,
        }
    }

    /// Re-highlight if the text changed since last time.
    pub fn refresh_highlight(&mut self) {
        if self.hl_version == self.version {
            return;
        }
        self.hl_version = self.version;
        self.hl = match self.lang {
            Some(l) => crate::syntax::highlight(l, &self.lines),
            None => vec![],
        };
    }

    pub fn text(&self) -> String {
        self.lines.join(if self.crlf { "\r\n" } else { "\n" })
    }

    pub fn modified(&self) -> bool {
        self.lines != self.saved
    }

    pub fn save(&mut self) -> Result<(), String> {
        std::fs::write(&self.path, self.text()).map_err(|e| e.to_string())?;
        self.saved = self.lines.clone();
        self.close_armed = false;
        Ok(())
    }

    pub fn name(&self) -> String {
        crate::util::file_name(&self.path)
    }

    // ---- positions ----

    fn line_len(&self, l: usize) -> usize {
        len(&self.lines[l])
    }

    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos::new(line, p.col.min(self.line_len(line)))
    }

    fn end(&self) -> Pos {
        let l = self.lines.len() - 1;
        Pos::new(l, self.line_len(l))
    }

    /// Display cell of a column (tabs expand to the next stop).
    pub fn x_of(&self, p: Pos) -> usize {
        let mut x = 0;
        for c in self.lines[p.line].chars().take(p.col) {
            x = if c == '\t' { (x / TAB + 1) * TAB } else { x + 1 };
        }
        x
    }

    /// Column nearest to display cell `x` on line `l`.
    pub fn col_at(&self, l: usize, x: usize) -> usize {
        let mut cx = 0;
        for (i, c) in self.lines[l].chars().enumerate() {
            let nx = if c == '\t' { (cx / TAB + 1) * TAB } else { cx + 1 };
            if x < nx {
                return if x - cx > (nx - cx) / 2 { i + 1 } else { i };
            }
            cx = nx;
        }
        self.line_len(l)
    }

    fn prev(&self, p: Pos) -> Pos {
        if p.col > 0 {
            Pos::new(p.line, p.col - 1)
        } else if p.line > 0 {
            Pos::new(p.line - 1, self.line_len(p.line - 1))
        } else {
            p
        }
    }

    fn next(&self, p: Pos) -> Pos {
        if p.col < self.line_len(p.line) {
            Pos::new(p.line, p.col + 1)
        } else if p.line + 1 < self.lines.len() {
            Pos::new(p.line + 1, 0)
        } else {
            p
        }
    }

    /// Start of the word before `p` (skipping whitespace, then one run of a class).
    fn word_left(&self, mut p: Pos) -> Pos {
        if p.col == 0 {
            return self.prev(p);
        }
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        while p.col > 0 && chars[p.col - 1].is_whitespace() {
            p.col -= 1;
        }
        if p.col > 0 {
            let k = class(chars[p.col - 1]);
            while p.col > 0 && class(chars[p.col - 1]) == k {
                p.col -= 1;
            }
        }
        p
    }

    fn word_right(&self, mut p: Pos) -> Pos {
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        if p.col >= chars.len() {
            return self.next(p);
        }
        while p.col < chars.len() && chars[p.col].is_whitespace() {
            p.col += 1;
        }
        if p.col < chars.len() {
            let k = class(chars[p.col]);
            while p.col < chars.len() && class(chars[p.col]) == k {
                p.col += 1;
            }
        }
        p
    }

    /// First non-blank column, or 0 if the cursor is already there (Home toggles).
    fn smart_home(&self, p: Pos) -> Pos {
        let ind = self.lines[p.line].chars().take_while(|c| c.is_whitespace()).count();
        Pos::new(p.line, if p.col == ind { 0 } else { ind })
    }

    // ---- selection ----

    /// The selection, ordered, if non-empty.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        (a != self.cur).then(|| if a < self.cur { (a, self.cur) } else { (self.cur, a) })
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(self.slice(a, b))
    }

    fn slice(&self, a: Pos, b: Pos) -> String {
        if a.line == b.line {
            let l = &self.lines[a.line];
            return l[byte(l, a.col)..byte(l, b.col)].to_string();
        }
        let mut s = self.lines[a.line][byte(&self.lines[a.line], a.col)..].to_string();
        for l in &self.lines[a.line + 1..b.line] {
            s.push('\n');
            s.push_str(l);
        }
        s.push('\n');
        s.push_str(&self.lines[b.line][..byte(&self.lines[b.line], b.col)]);
        s
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(Pos::default());
        self.cur = self.end();
    }

    /// Select the word under `p` (double-click).
    pub fn select_word(&mut self, p: Pos) {
        let p = self.clamp(p);
        let chars: Vec<char> = self.lines[p.line].chars().collect();
        let Some(&c) = chars.get(p.col).or_else(|| p.col.checked_sub(1).and_then(|i| chars.get(i))) else { return self.set_cursor(p, false) };
        let k = class(c);
        let mut a = p.col.min(chars.len() - 1);
        let mut b = a;
        while a > 0 && class(chars[a - 1]) == k {
            a -= 1;
        }
        while b < chars.len() && class(chars[b]) == k {
            b += 1;
        }
        self.anchor = Some(Pos::new(p.line, a));
        self.cur = Pos::new(p.line, b);
    }

    /// Place the cursor; `extend` keeps (or starts) a selection.
    pub fn set_cursor(&mut self, p: Pos, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cur);
        } else {
            self.anchor = None;
        }
        self.cur = self.clamp(p);
        self.want_x = None;
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    // ---- motion ----

    pub fn move_to(&mut self, m: Move, extend: bool) {
        // Without shift, left/right on a selection collapse it to that side.
        if !extend {
            if let Some((a, b)) = self.selection() {
                match m {
                    Move::Left => return self.set_cursor(a, false),
                    Move::Right => return self.set_cursor(b, false),
                    _ => {}
                }
            }
        }
        let c = self.cur;
        let vertical = |e: &mut Self, dl: isize| {
            let x = e.want_x.unwrap_or_else(|| e.x_of(c));
            let l = c.line as isize + dl;
            let p = if l < 0 {
                Pos::default()
            } else if l as usize >= e.lines.len() {
                e.end()
            } else {
                Pos::new(l as usize, e.col_at(l as usize, x))
            };
            (p, x)
        };
        let (p, keep_x) = match m {
            Move::Left => (self.prev(c), None),
            Move::Right => (self.next(c), None),
            Move::WordLeft => (self.word_left(c), None),
            Move::WordRight => (self.word_right(c), None),
            Move::Home => (self.smart_home(c), None),
            Move::End => (Pos::new(c.line, self.line_len(c.line)), None),
            Move::Up(n) => {
                let (p, x) = vertical(self, -(n as isize));
                (p, Some(x))
            }
            Move::Down(n) => {
                let (p, x) = vertical(self, n as isize);
                (p, Some(x))
            }
            Move::DocStart => (Pos::default(), None),
            Move::DocEnd => (self.end(), None),
        };
        self.set_cursor(p, extend);
        self.want_x = keep_x;
    }

    // ---- editing ----

    fn checkpoint(&mut self, kind: EditKind) {
        let now = Instant::now();
        let join = kind != EditKind::Other && self.last_edit.is_some_and(|(k, t)| k == kind && now - t < UNDO_JOIN);
        if !join {
            self.undo.push(Snapshot { lines: self.lines.clone(), cur: self.cur });
            if self.undo.len() > 500 {
                self.undo.remove(0);
            }
        }
        self.last_edit = Some((kind, now));
        self.version += 1;
        self.redo.clear();
        self.close_armed = false;
    }

    /// Remove `a..b` (ordered) and put the cursor at `a`.
    fn remove(&mut self, a: Pos, b: Pos) {
        let tail = self.lines[b.line][byte(&self.lines[b.line], b.col)..].to_string();
        let l = &mut self.lines[a.line];
        l.truncate(byte(l, a.col));
        l.push_str(&tail);
        self.lines.drain(a.line + 1..=b.line);
        self.cur = a;
        self.anchor = None;
        self.want_x = None;
    }

    /// Insert `s` at the cursor, replacing the selection.
    fn insert_raw(&mut self, s: &str) {
        if let Some((a, b)) = self.selection() {
            self.remove(a, b);
        }
        self.anchor = None;
        let c = self.cur;
        let line = &self.lines[c.line];
        let at = byte(line, c.col);
        let tail = line[at..].to_string();
        let head = line[..at].to_string();
        let mut parts = s.split('\n').map(|p| p.strip_suffix('\r').unwrap_or(p));
        let first = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();
        if rest.is_empty() {
            self.lines[c.line] = format!("{head}{first}{tail}");
            self.cur = Pos::new(c.line, c.col + len(first));
        } else {
            self.lines[c.line] = format!("{head}{first}");
            let last = rest.len() - 1;
            let new: Vec<String> = rest.iter().enumerate().map(|(i, p)| if i == last { format!("{p}{tail}") } else { p.to_string() }).collect();
            self.cur = Pos::new(c.line + rest.len(), len(rest[last]));
            self.lines.splice(c.line + 1..c.line + 1, new);
        }
        self.want_x = None;
    }

    /// Typed text (one undo step per burst of typing).
    pub fn type_str(&mut self, s: &str) {
        let kind = if self.selection().is_some() || s.contains('\n') { EditKind::Other } else { EditKind::Insert };
        self.checkpoint(kind);
        self.insert_raw(s);
    }

    /// Pasted or otherwise inserted text: its own undo step.
    pub fn insert(&mut self, s: &str) {
        self.checkpoint(EditKind::Other);
        self.insert_raw(s);
    }

    /// Enter: new line keeping the current indentation.
    pub fn newline(&mut self) {
        let ind: String = self.lines[self.cur.line].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let ind = if self.selection().is_none() && self.cur.col < len(&ind) { ind.chars().take(self.cur.col).collect() } else { ind };
        self.insert(&format!("\n{ind}"));
    }

    /// The file's indent unit: a tab if any line starts with one, else spaces.
    fn indent_unit(&self) -> String {
        if self.lines.iter().any(|l| l.starts_with('\t')) {
            "\t".into()
        } else {
            " ".repeat(TAB)
        }
    }

    /// Tab: indent the selected lines, else insert to the next tab stop.
    pub fn tab(&mut self) {
        let unit = self.indent_unit();
        match self.selection() {
            Some((a, b)) if a.line != b.line => self.indent(1),
            _ if unit == "\t" => self.type_str("\t"),
            _ => {
                let x = self.x_of(self.cur);
                self.type_str(&" ".repeat(TAB - x % TAB));
            }
        }
    }

    /// Indent (`dir` > 0) or dedent the lines touched by the cursor/selection.
    pub fn indent(&mut self, dir: i32) {
        let (a, b) = self.selection().unwrap_or((self.cur, self.cur));
        let last = if b.line > a.line && b.col == 0 { b.line - 1 } else { b.line };
        let unit = self.indent_unit();
        self.checkpoint(EditKind::Other);
        let shift = |e: &mut Self, l: usize| -> isize {
            if dir > 0 {
                if e.lines[l].is_empty() {
                    return 0;
                }
                e.lines[l].insert_str(0, &unit);
                len(&unit) as isize
            } else {
                let n = if e.lines[l].starts_with('\t') { 1 } else { e.lines[l].chars().take(TAB).take_while(|c| *c == ' ').count() };
                e.lines[l].drain(..n);
                -(n as isize)
            }
        };
        let mut moved = (0isize, 0isize);
        for l in a.line..=last {
            let d = shift(self, l);
            if l == self.cur.line {
                moved.0 = d;
            }
            if Some(l) == self.anchor.map(|p| p.line) {
                moved.1 = d;
            }
        }
        let adj = |p: Pos, d: isize, e: &Self| e.clamp(Pos::new(p.line, (p.col as isize + d).max(0) as usize));
        self.cur = adj(self.cur, moved.0, self);
        self.anchor = self.anchor.map(|p| adj(p, moved.1, self));
    }

    /// Backspace / Delete over the selection, else the span `m` covers from the cursor.
    pub fn delete(&mut self, m: Move) {
        if let Some((a, b)) = self.selection() {
            self.checkpoint(EditKind::Other);
            return self.remove(a, b);
        }
        let c = self.cur;
        let p = match m {
            Move::Left => {
                // In leading spaces, Backspace removes back to the previous tab stop.
                let line = &self.lines[c.line];
                let x = self.x_of(c);
                let blank = line.chars().take(c.col).all(|ch| ch == ' ');
                if blank && c.col > 0 && !line.starts_with('\t') {
                    Pos::new(c.line, c.col - ((x - 1) % TAB + 1).min(c.col))
                } else {
                    self.prev(c)
                }
            }
            Move::Right => self.next(c),
            Move::WordLeft => self.word_left(c),
            Move::WordRight => self.word_right(c),
            Move::Home => Pos::new(c.line, 0),
            Move::End => Pos::new(c.line, self.line_len(c.line)),
            _ => return,
        };
        if p == c {
            return;
        }
        self.checkpoint(EditKind::Delete);
        let (a, b) = if p < c { (p, c) } else { (c, p) };
        self.remove(a, b);
    }

    /// Cut: the selection, else the whole line (as Zed does).
    pub fn cut(&mut self) -> String {
        if let Some(s) = self.selected_text() {
            self.delete(Move::Left);
            return s;
        }
        let l = self.cur.line;
        self.checkpoint(EditKind::Other);
        let s = format!("{}\n", self.lines[l]);
        if self.lines.len() > 1 {
            self.lines.remove(l);
        } else {
            self.lines[0].clear();
        }
        self.cur = self.clamp(Pos::new(l, 0));
        s
    }

    /// Copy: the selection, else the whole line.
    pub fn copy(&self) -> String {
        self.selected_text().unwrap_or_else(|| format!("{}\n", self.lines[self.cur.line]))
    }

    pub fn undo(&mut self) {
        self.swap(true);
    }

    pub fn redo(&mut self) {
        self.swap(false);
    }

    fn swap(&mut self, undo: bool) {
        let (from, to) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        let Some(s) = from.pop() else { return };
        to.push(Snapshot { lines: std::mem::replace(&mut self.lines, s.lines), cur: self.cur });
        self.cur = self.clamp(s.cur);
        self.anchor = None;
        self.want_x = None;
        self.last_edit = None;
        self.version += 1;
    }

    // ---- scrolling ----

    /// Keep the cursor inside a `rows` × `cols` viewport, with a small margin.
    pub fn reveal(&mut self, rows: usize, cols: usize) {
        let m = 3.min(rows.saturating_sub(1) / 2);
        let l = self.cur.line;
        if l < self.top + m {
            self.top = l.saturating_sub(m);
        } else if l + m + 1 > self.top + rows {
            self.top = (l + m + 1).saturating_sub(rows);
        }
        let x = self.x_of(self.cur);
        if x < self.left {
            self.left = x.saturating_sub(4);
        } else if x + 1 > self.left + cols {
            self.left = x + 5 - cols.min(x + 5);
        }
        self.clamp_top();
    }

    pub fn scroll(&mut self, lines: isize) {
        self.top = (self.top as isize + lines).max(0) as usize;
        self.clamp_top();
    }

    /// Scroll no further than the last line at the top (as Zed does).
    fn clamp_top(&mut self) {
        self.top = self.top.min(self.lines.len().saturating_sub(1));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Move {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    Up(usize),
    Down(usize),
    DocStart,
    DocEnd,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(s: &str) -> Editor {
        Editor::from_text(PathBuf::from("/tmp/x"), s)
    }

    #[test]
    fn insert_and_newline_keep_indent() {
        let mut e = ed("    foo");
        e.move_to(Move::End, false);
        e.newline();
        e.type_str("bar");
        assert_eq!(e.text(), "    foo\n    bar");
        assert!(e.modified());
        e.undo();
        assert_eq!(e.text(), "    foo\n    ");
        e.undo();
        assert_eq!(e.text(), "    foo");
        assert!(!e.modified());
        e.redo();
        e.redo();
        assert_eq!(e.text(), "    foo\n    bar");
    }

    #[test]
    fn multiline_insert_and_delete_selection() {
        let mut e = ed("abc\ndef");
        e.set_cursor(Pos::new(0, 1), false);
        e.insert("X\nY\nZ");
        assert_eq!(e.text(), "aX\nY\nZbc\ndef");
        assert_eq!(e.cur, Pos::new(2, 1));
        e.set_cursor(Pos::new(0, 1), false);
        e.set_cursor(Pos::new(3, 1), true);
        assert_eq!(e.selected_text().unwrap(), "X\nY\nZbc\nd");
        e.delete(Move::Left);
        assert_eq!(e.text(), "aef");
    }

    #[test]
    fn word_motion_and_backspace() {
        let mut e = ed("let foo_bar = 1;");
        e.move_to(Move::End, false);
        e.move_to(Move::WordLeft, false);
        assert_eq!(e.cur.col, 15);
        e.move_to(Move::WordLeft, false);
        assert_eq!(e.cur.col, 14);
        e.move_to(Move::WordLeft, false);
        e.move_to(Move::WordLeft, false);
        assert_eq!(e.cur.col, 4);
        e.delete(Move::WordRight);
        assert_eq!(e.text(), "let  = 1;");
    }

    #[test]
    fn tabs_and_columns() {
        let mut e = ed("\tx\nabcdefgh");
        assert_eq!(e.x_of(Pos::new(0, 1)), 4);
        e.set_cursor(Pos::new(1, 6), false);
        e.move_to(Move::Up(1), false);
        assert_eq!(e.cur, Pos::new(0, 2));
        e.move_to(Move::Down(1), false);
        assert_eq!(e.cur, Pos::new(1, 6), "keeps the wanted column");
    }

    #[test]
    fn indent_dedent_and_soft_backspace() {
        let mut e = ed("a\nb");
        e.select_all();
        e.indent(1);
        assert_eq!(e.text(), "    a\n    b");
        e.indent(-1);
        assert_eq!(e.text(), "a\nb");
        let mut e = ed("        x");
        e.set_cursor(Pos::new(0, 8), false);
        e.delete(Move::Left);
        assert_eq!(e.text(), "    x");
    }

    #[test]
    fn crlf_roundtrip_and_cut_line() {
        let mut e = ed("a\r\nb\r\n");
        assert_eq!(e.lines, vec!["a", "b", ""]);
        assert_eq!(e.text(), "a\r\nb\r\n");
        assert_eq!(e.cut(), "a\n");
        assert_eq!(e.text(), "b\r\n");
    }

    #[test]
    fn select_word_unicode() {
        let mut e = ed("héllo wörld");
        e.select_word(Pos::new(0, 8));
        assert_eq!(e.selected_text().unwrap(), "wörld");
    }
}
