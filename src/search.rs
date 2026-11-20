//! "Find in project" (⌘⇧F): search every file of a worktree for a pattern, like Zed's project
//! search. Matching runs on worker threads; a newer search cancels the one in flight.
use regex::{Regex, RegexBuilder};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Stop collecting after this many matching lines.
pub const MAX_HITS: usize = 10_000;
/// Files bigger than this are skipped (minified bundles, data dumps).
const MAX_FILE: u64 = 4 << 20;
/// Chars of a matching line kept for display.
const LINE_KEEP: usize = 400;

/// What to look for.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

impl Query {
    pub fn build(&self) -> Result<Regex, String> {
        let pat = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        let pat = if self.word { format!(r"\b(?:{pat})\b") } else { pat };
        RegexBuilder::new(&pat).case_insensitive(!self.case).multi_line(true).build().map_err(|e| e.to_string().lines().last().unwrap_or("bad pattern").trim().to_string())
    }
}

/// A matching line.
pub struct Hit {
    /// 0-based line number.
    pub line: usize,
    /// The line (cut to `LINE_KEEP` chars), tabs kept.
    pub text: String,
    /// Matched char ranges in `text`.
    pub ranges: Vec<(usize, usize)>,
}

/// One file's matches.
pub struct FileHits {
    /// Relative to the search root, `/`-separated.
    pub path: String,
    pub hits: Vec<Hit>,
}

/// The result of one search.
pub struct Found {
    pub files: Vec<FileHits>,
    /// Hit `MAX_HITS`: there are more.
    pub truncated: bool,
}

/// Search `root`'s files (git's view, see `finder::list`) for `re`. Blocking; returns `None` when
/// `cancel` got set meanwhile.
pub fn run(root: &Path, re: &Regex, cancel: &AtomicBool) -> Option<Found> {
    let mut files = crate::finder::list(root);
    files.sort_unstable();
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let next = AtomicUsize::new(0);
    let count = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, FileHits)>> = Mutex::new(Vec::new());
    let n = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    std::thread::scope(|s| {
        for _ in 0..n {
            s.spawn(|| loop {
                if cancel.load(Ordering::Relaxed) || count.load(Ordering::Relaxed) >= MAX_HITS {
                    return;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(f) = files.get(i) else { return };
                let hits = search_file(&root.join(f), re);
                if !hits.is_empty() {
                    count.fetch_add(hits.len(), Ordering::Relaxed);
                    out.lock().unwrap().push((i, FileHits { path: f.clone(), hits }));
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let mut v = out.into_inner().unwrap();
    v.sort_unstable_by_key(|(i, _)| *i);
    let truncated = count.load(Ordering::Relaxed) >= MAX_HITS;
    Some(Found { files: v.into_iter().map(|(_, f)| f).collect(), truncated })
}

/// Matching lines of one file; none for binaries, big or unreadable files.
fn search_file(p: &Path, re: &Regex) -> Vec<Hit> {
    let Ok(meta) = std::fs::metadata(p) else { return vec![] };
    if !meta.is_file() || meta.len() > MAX_FILE {
        return vec![];
    }
    let Ok(bytes) = std::fs::read(p) else { return vec![] };
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return vec![];
    }
    let text = String::from_utf8_lossy(&bytes);
    search_text(&text, re)
}

pub fn search_text(text: &str, re: &Regex) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    // Line starts, found lazily as matches move forward.
    let (mut line, mut line_start) = (0usize, 0usize);
    let mut scanned = 0usize;
    for m in re.find_iter(text) {
        if m.start() == m.end() {
            continue;
        }
        line += text[scanned..m.start()].bytes().filter(|b| *b == b'\n').count();
        if let Some(nl) = text[scanned..m.start()].rfind('\n') {
            line_start = scanned + nl + 1;
        }
        scanned = m.start();
        let line_end = text[line_start..].find('\n').map_or(text.len(), |e| line_start + e);
        let raw = text[line_start..line_end].trim_end_matches('\r');
        // A match spanning lines (regex with \n) is shown on its first line.
        let a = text[line_start..m.start()].chars().count();
        let b = a + text[m.start()..m.end().min(line_start + raw.len()).max(m.start())].chars().count();
        if a >= LINE_KEEP {
            continue;
        }
        let b = b.min(LINE_KEEP).max(a + 1);
        match hits.last_mut() {
            Some(h) if h.line == line => h.ranges.push((a, b)),
            _ => hits.push(Hit { line, text: raw.chars().take(LINE_KEEP).collect(), ranges: vec![(a, b)] }),
        }
    }
    hits
}

/// A row of the results list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    /// File `i`'s header.
    File(usize),
    /// Hit `j` of file `i`.
    Hit(usize, usize),
}

/// State of a search tab.
pub struct ProjectSearch {
    pub root: PathBuf,
    pub query: Query,
    /// The query the shown results are for (`None`: nothing searched yet).
    pub searched: Option<Query>,
    pub found: Option<Found>,
    /// Flattened results: a header per file, then its hits.
    pub rows: Vec<Row>,
    /// Selected row (always a `Row::Hit` when there are hits).
    pub sel: usize,
    /// First visible row.
    pub top: usize,
    pub error: Option<String>,
    /// The search in flight, and its cancel flag.
    pub running: Option<(u64, Arc<AtomicBool>)>,
    pub hits: usize,
}

impl ProjectSearch {
    pub fn new(root: PathBuf, text: String) -> Self {
        ProjectSearch {
            root,
            query: Query { text, ..Default::default() },
            searched: None,
            found: None,
            rows: vec![],
            sel: 0,
            top: 0,
            error: None,
            running: None,
            hits: 0,
        }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        match &self.searched {
            Some(q) if !q.text.is_empty() => format!("Search: {}", q.text),
            _ => "Project Search".into(),
        }
    }

    /// Results arrived for `q`.
    pub fn set_found(&mut self, q: Query, f: Found) {
        self.rows.clear();
        self.hits = 0;
        for (i, fh) in f.files.iter().enumerate() {
            self.rows.push(Row::File(i));
            self.rows.extend((0..fh.hits.len()).map(|j| Row::Hit(i, j)));
            self.hits += fh.hits.len();
        }
        self.found = Some(f);
        self.searched = Some(q);
        self.running = None;
        self.top = 0;
        self.sel = self.rows.iter().position(|r| matches!(r, Row::Hit(..))).unwrap_or(0);
    }

    pub fn hit(&self, row: usize) -> Option<(&FileHits, &Hit)> {
        let Row::Hit(i, j) = *self.rows.get(row)? else { return None };
        let f = &self.found.as_ref()?.files[i];
        Some((f, &f.hits[j]))
    }

    /// Move the selection `d` hits (skipping file headers), clamped, keeping it in `vis` rows.
    pub fn step(&mut self, d: isize, vis: usize) {
        let hits: Vec<usize> = (0..self.rows.len()).filter(|&r| matches!(self.rows[r], Row::Hit(..))).collect();
        if hits.is_empty() {
            return;
        }
        let k = hits.partition_point(|&r| r < self.sel) as isize;
        self.sel = hits[(k + d).clamp(0, hits.len() as isize - 1) as usize];
        self.show_sel(vis);
    }

    /// Scroll so the selection shows (with its file header when that's just above).
    pub fn show_sel(&mut self, vis: usize) {
        let vis = vis.max(1);
        let want = if self.sel > 0 && matches!(self.rows[self.sel - 1], Row::File(_)) { self.sel - 1 } else { self.sel };
        if want < self.top {
            self.top = want;
        } else if self.sel >= self.top + vis {
            self.top = self.sel + 1 - vis;
        }
    }

    pub fn scroll(&mut self, d: isize, vis: usize) {
        let max = self.rows.len().saturating_sub(vis);
        self.top = (self.top as isize + d).clamp(0, max as isize) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> Query {
        Query { text: text.into(), ..Default::default() }
    }

    #[test]
    fn finds_lines_and_ranges() {
        let re = q("foo").build().unwrap();
        let h = search_text("a foo\nbar\nFoo foo\n", &re);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].line, &h[0].ranges[..]), (0, &[(2, 5)][..]));
        assert_eq!((h[1].line, &h[1].ranges[..]), (2, &[(0, 3), (4, 7)][..]));
        let re = Query { case: true, ..q("Foo") }.build().unwrap();
        assert_eq!(search_text("foo\nFoo", &re).len(), 1);
        let re = Query { word: true, ..q("foo") }.build().unwrap();
        assert_eq!(search_text("foobar\nfoo.bar", &re)[0].line, 1);
        let re = Query { regex: true, ..q("a.c") }.build().unwrap();
        assert_eq!(search_text("abc\na.c", &re).len(), 2);
        assert!(Query { regex: true, ..q("(") }.build().is_err());
        // Literal by default.
        assert_eq!(search_text("abc\na.c", &q("a.c").build().unwrap()).len(), 1);
        // Char columns, not bytes.
        assert_eq!(search_text("é foo", &re_of("foo"))[0].ranges, vec![(2, 5)]);
    }

    fn re_of(s: &str) -> Regex {
        q(s).build().unwrap()
    }

    #[test]
    fn searches_this_repo() {
        let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        let f = run(&root, &re_of("fn searches_this_repo"), &AtomicBool::new(false)).unwrap();
        assert!(f.files.iter().any(|f| f.path == "src/search.rs"));
        let mut s = ProjectSearch::new(root, String::new());
        s.set_found(q("x"), f);
        assert!(matches!(s.rows[s.sel], Row::Hit(..)));
    }
}
