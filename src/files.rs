//! File explorer for the current view, laid out like Zed's project panel: a lazily read tree,
//! folders first, git status colors, live updates from watching the expanded directories.
use crate::git::{self, FileState};
use crate::util;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Hidden like Zed's default `file_scan_exclusions`.
const EXCLUDE: &[&str] = &[".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db"];

/// One visible explorer row.
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    /// Where a symbolic link points (resolved when possible); `dir` follows it.
    pub link: Option<PathBuf>,
    pub depth: usize,
}

/// The file tree of one view root: which folders are open, the flattened rows, selection and scroll.
pub struct Explorer {
    pub root: PathBuf,
    /// Expanded folders.
    pub open: HashSet<PathBuf>,
    /// Visible rows, flattened.
    pub entries: Vec<Entry>,
    pub sel: usize,
    /// First visible row.
    pub top: usize,
    pub status: git::Status,
    /// Last left click (row, time), for double-click.
    pub last_click: Option<(usize, Instant)>,
}

impl Explorer {
    pub fn new(root: PathBuf) -> Self {
        let mut e = Explorer { open: HashSet::from([root.clone()]), root, entries: vec![], sel: 0, top: 0, status: git::Status::default(), last_click: None };
        e.rebuild();
        e
    }

    /// Re-read the expanded folders. Keeps the selection on the same path when it still exists.
    pub fn rebuild(&mut self) {
        let keep = self.entries.get(self.sel).map(|e| e.path.clone());
        let mut out = vec![Entry { path: self.root.clone(), name: util::file_name(&self.root), dir: true, link: None, depth: 0 }];
        if self.open.contains(&self.root) {
            walk(&self.root, 1, &self.open, &mut out);
        }
        self.entries = out;
        if let Some(i) = keep.and_then(|k| self.entries.iter().position(|e| e.path == k)) {
            self.sel = i;
        }
        self.sel = self.sel.min(self.entries.len() - 1);
    }

    /// Expand or collapse folder row `i`.
    pub fn toggle(&mut self, i: usize) {
        let Some(e) = self.entries.get(i).filter(|e| e.dir) else { return };
        util::toggle(&mut self.open, e.path.clone());
        self.rebuild();
    }

    pub fn is_open(&self, i: usize) -> bool {
        self.entries.get(i).is_some_and(|e| e.dir && self.open.contains(&e.path))
    }

    /// Row of the folder containing row `i`.
    pub fn parent(&self, i: usize) -> Option<usize> {
        let d = self.entries.get(i)?.depth;
        (0..i).rev().find(|&k| self.entries[k].depth < d)
    }

    pub fn git(&self, path: &Path) -> Option<FileState> {
        self.status.get(&self.root, path)
    }

    /// Folders to watch: the root and every expanded folder that still exists.
    pub fn watch_dirs(&self) -> Vec<PathBuf> {
        self.entries.iter().filter(|e| e.dir && self.open.contains(&e.path)).map(|e| e.path.clone()).collect()
    }

    /// Keep the selection inside `vis` visible rows.
    pub fn reveal_sel(&mut self, vis: usize) {
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + vis {
            self.top = self.sel + 1 - vis;
        }
        self.clamp_top(vis);
    }

    pub fn clamp_top(&mut self, vis: usize) {
        self.top = self.top.min(self.entries.len().saturating_sub(vis));
    }
}

fn walk(dir: &Path, depth: usize, open: &HashSet<PathBuf>, out: &mut Vec<Entry>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut items: Vec<(String, bool, Option<PathBuf>)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if EXCLUDE.contains(&name.as_str()) {
                return None;
            }
            let ft = e.file_type().ok()?;
            let link = ft.is_symlink().then(|| std::fs::canonicalize(e.path()).or_else(|_| std::fs::read_link(e.path())).unwrap_or_default());
            let is_dir = if link.is_some() { e.path().is_dir() } else { ft.is_dir() };
            Some((name, is_dir, link))
        })
        .collect();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())).then_with(|| a.0.cmp(&b.0)));
    for (name, is_dir, link) in items {
        let path = dir.join(&name);
        let expand = is_dir && open.contains(&path);
        out.push(Entry { path: path.clone(), name, dir: is_dir, link, depth });
        if expand {
            walk(&path, depth + 1, open, out);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tree_and_status_of_this_repo() {
        let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut e = super::Explorer::new(root.clone());
        let names: Vec<_> = e.entries.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"src") && names.contains(&"Cargo.toml") && !names.contains(&".git"));
        let src = e.entries.iter().position(|x| x.name == "src").unwrap();
        assert!(src < e.entries.iter().position(|x| x.name == "Cargo.toml").unwrap(), "folders first");
        e.toggle(src);
        assert!(e.entries.iter().any(|x| x.name == "files.rs" && x.depth == 2));
        e.status = crate::git::status(&root);
        println!("{:?}", e.status.trees);
        assert_eq!(e.git(&root.join("target/release")), Some(super::FileState::Ignored));
    }
}
