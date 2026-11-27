//! "Go to file" (⌘P): the view's files, fuzzy matched against a query, like Zed's file finder.
use std::path::{Path, PathBuf};
use std::process::Command;

/// Skipped when the folder isn't a git repo (in a repo, `.gitignore` decides).
const SKIP_DIRS: &[&str] = &[".git", ".svn", ".hg", "node_modules", "target", ".DS_Store"];
/// Stop indexing a non-git folder after this many files.
const WALK_CAP: usize = 100_000;
/// Results kept per query.
const MAX_RESULTS: usize = 200;

/// One fuzzy match of the query against a file path.
pub struct Match {
    /// Index into `Finder::files`.
    pub file: usize,
    /// Char indices of the matched characters in the file's relative path.
    pub pos: Vec<usize>,
    score: i32,
}

/// State of the open ⌘P popup: the indexed files, the query, and its ranked matches.
pub struct Finder {
    pub root: PathBuf,
    /// Paths relative to `root`, `/`-separated.
    pub files: Vec<String>,
    /// Recently opened files (indices into `files`), most recent first: shown for an empty query.
    recent: Vec<usize>,
    pub query: String,
    pub matches: Vec<Match>,
    pub sel: usize,
    /// First visible result.
    pub top: usize,
    pub hover: Option<usize>,
    /// The file list is still being read (`list` runs on a thread: `git ls-files` of a big repo
    /// takes a moment, and the popup shows at once).
    pub loading: bool,
}

impl Finder {
    /// Index `root` now (blocking); `recent` are absolute paths, most recent first.
    #[cfg(test)]
    pub fn new(root: &Path, recent: &[PathBuf]) -> Self {
        let mut f = Self::loading(root.to_path_buf());
        f.set_files(list(root), recent);
        f
    }

    /// An empty popup for `root`, waiting for `set_files` (from `list` on another thread).
    pub fn loading(root: PathBuf) -> Self {
        Finder {
            root,
            files: vec![],
            recent: vec![],
            query: String::new(),
            matches: vec![],
            sel: 0,
            top: 0,
            hover: None,
            loading: true,
        }
    }

    /// The file list arrived: match the query typed so far against it.
    pub fn set_files(&mut self, mut files: Vec<String>, recent: &[PathBuf]) {
        files.sort_unstable();
        self.recent = recent
            .iter()
            .filter_map(|p| p.strip_prefix(&self.root).ok())
            .filter_map(|r| files.binary_search(&r.to_string_lossy().into_owned()).ok())
            .collect();
        self.files = files;
        self.loading = false;
        self.update();
    }

    pub fn path(&self, m: usize) -> Option<PathBuf> {
        Some(self.root.join(&self.files[self.matches.get(m)?.file]))
    }

    /// Re-match after the query changed.
    pub fn update(&mut self) {
        let q: Vec<char> = self.query.chars().filter(|c| !c.is_whitespace()).collect();
        self.sel = 0;
        self.top = 0;
        if q.is_empty() {
            let rest = (0..self.files.len()).filter(|i| !self.recent.contains(i));
            self.matches = self
                .recent
                .iter()
                .copied()
                .chain(rest)
                .take(MAX_RESULTS)
                .map(|file| Match { file, pos: vec![], score: 0 })
                .collect();
            return;
        }
        let mut m: Vec<Match> = self
            .files
            .iter()
            .enumerate()
            .filter_map(|(file, f)| score(&q, f).map(|(score, pos)| Match { file, pos, score }))
            .collect();
        m.sort_unstable_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| self.files[a.file].len().cmp(&self.files[b.file].len()))
                .then_with(|| a.file.cmp(&b.file))
        });
        m.truncate(MAX_RESULTS);
        self.matches = m;
    }

    /// Move the selection by `d`, wrapping, keeping it inside `vis` visible rows.
    pub fn step(&mut self, d: isize, vis: usize) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.sel = (self.sel as isize + d).rem_euclid(n as isize) as usize;
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + vis {
            self.top = self.sel + 1 - vis;
        }
    }

    pub fn scroll(&mut self, d: isize, vis: usize) {
        let max = self.matches.len().saturating_sub(vis);
        self.top = (self.top as isize + d).clamp(0, max as isize) as usize;
    }
}

/// Files under `root`: git's view (tracked + untracked, minus ignored) in a repo, else a walk.
/// Blocking: call off the UI thread.
pub fn list(root: &Path) -> Vec<String> {
    let git = |args: &[&str]| {
        let o = Command::new("git").arg("-C").arg(root).args(args).output().ok().filter(|o| o.status.success())?;
        Some(
            String::from_utf8_lossy(&o.stdout)
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect::<Vec<_>>(),
        )
    };
    if let Some(files) = git(&["ls-files", "-z", "-co", "--exclude-standard", "--deduplicate"]) {
        // Drop what isn't an openable file, without a stat per file: tracked files deleted from
        // disk, and submodules (gitlinks, mode 160000).
        let mut gone: std::collections::HashSet<String> =
            git(&["ls-files", "-z", "-d"]).unwrap_or_default().into_iter().collect();
        let staged = git(&["ls-files", "-z", "-s"]).unwrap_or_default();
        gone.extend(
            staged.iter().filter(|l| l.starts_with("160000 ")).filter_map(|l| Some(l.split_once('\t')?.1.to_string())),
        );
        return files.into_iter().filter(|f| !gone.contains(f)).collect();
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if out.len() >= WALK_CAP {
            return;
        }
        let name = e.file_name();
        if SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            walk(root, &p, out);
        } else if ft.is_file() {
            if let Ok(r) = p.strip_prefix(root) {
                out.push(r.to_string_lossy().into_owned());
            }
        }
    }
}

const HIT: i32 = 16;
const AFTER_SLASH: i32 = 12;
const AFTER_SEP: i32 = 9;
const CAMEL: i32 = 7;
const CONSECUTIVE: i32 = 8;
const GAP: i32 = -3;
const IN_NAME: i32 = 4;
const SAME_CASE: i32 = 1;

/// Fuzzy score of `q` (no whitespace) against path `s`, case-insensitive; `None` if `q` isn't a
/// subsequence. Rewards matches at word starts, runs, and the file name. Returns matched char indices.
pub fn score(q: &[char], s: &str) -> Option<(i32, Vec<usize>)> {
    // Quick subsequence check before the DP.
    let mut it = s.chars();
    if !q.iter().all(|qc| it.any(|c| eq(*qc, c))) {
        return None;
    }
    let cs: Vec<char> = s.chars().collect();
    let (m, n) = (q.len(), cs.len());
    let name_at = s.rfind('/').map_or(0, |b| s[..=b].chars().count());
    let bonus: Vec<i32> = (0..n)
        .map(|j| {
            let prev = if j == 0 { '/' } else { cs[j - 1] };
            let b = match prev {
                '/' => AFTER_SLASH,
                '_' | '-' | '.' | ' ' => AFTER_SEP,
                p if p.is_lowercase() && cs[j].is_uppercase() => CAMEL,
                _ => 0,
            };
            b + if j >= name_at { IN_NAME } else { 0 }
        })
        .collect();
    const NONE: i32 = i32::MIN / 2;
    // dp[i*n + j]: best score with q[i] matched at s[j]; from[i*n + j]: where q[i-1] matched.
    let mut dp = vec![NONE; m * n];
    let mut from = vec![usize::MAX; m * n];
    for i in 0..m {
        // Best dp[i-1][k] for k < j-1, and its k.
        let mut best = (NONE, usize::MAX);
        for j in i..n {
            if i > 0 && j >= 2 && dp[(i - 1) * n + j - 2] > best.0 {
                best = (dp[(i - 1) * n + j - 2], j - 2);
            }
            if !eq(q[i], cs[j]) {
                continue;
            }
            let here = HIT + bonus[j] + if q[i] == cs[j] { SAME_CASE } else { 0 };
            if i == 0 {
                dp[j] = here;
                continue;
            }
            let cons = if j >= 1 { dp[(i - 1) * n + j - 1] } else { NONE };
            let (via_cons, via_gap) = (cons + CONSECUTIVE, best.0 + GAP);
            let (s, k) = if cons > NONE && via_cons >= via_gap {
                (via_cons, j - 1)
            } else if best.0 > NONE {
                (via_gap, best.1)
            } else {
                continue;
            };
            dp[i * n + j] = s + here;
            from[i * n + j] = k;
        }
    }
    let (end, &best) = dp[(m - 1) * n..].iter().enumerate().max_by_key(|(j, v)| (**v, usize::MAX - j))?;
    if best <= NONE {
        return None;
    }
    let mut pos = vec![0; m];
    let mut j = end;
    for i in (0..m).rev() {
        pos[i] = j;
        j = from[i * n + j];
    }
    Some((best - n as i32 / 8, pos))
}

fn eq(q: char, c: char) -> bool {
    q == c || q.to_lowercase().eq(c.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::score;

    fn s(q: &str, p: &str) -> Option<i32> {
        score(&q.chars().collect::<Vec<_>>(), p).map(|x| x.0)
    }

    #[test]
    fn matches_and_ranks() {
        assert!(s("xyz", "src/main.rs").is_none());
        assert!(s("mainrs", "src/main.rs").is_some());
        // File name beats a scattered path match.
        assert!(s("main", "src/main.rs") > s("main", "maybe/a/index/nested.rs"));
        // Word starts beat mid-word.
        assert!(s("ar", "src/app/restore.rs") > s("ar", "src/bar.rs"));
        let (_, pos) = score(&"fin".chars().collect::<Vec<_>>(), "src/app/finder.rs").unwrap();
        assert_eq!(pos, vec![8, 9, 10]);
    }

    #[test]
    fn indexes_this_repo() {
        let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut f = super::Finder::new(&root, &[]);
        assert!(f.files.iter().any(|p| p == "src/finder.rs"));
        assert!(!f.files.iter().any(|p| p.starts_with("target/")));
        f.query = "findrs".into();
        f.update();
        assert_eq!(f.files[f.matches[0].file], "src/finder.rs");
    }
}
