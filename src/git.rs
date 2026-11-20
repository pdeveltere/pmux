//! Git: worktree discovery by reading .git files directly (no subprocess), plus the few
//! operations that do shell out to `git` (add/remove worktrees, status, behind counts).
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub main: bool,
}

/// Git state of a path. Order is severity: a folder takes the worst state inside it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum FileState {
    Ignored,
    Added,
    Deleted,
    Modified,
    Conflict,
}

/// `git status` of a checkout, by absolute path.
#[derive(Default)]
pub struct Status {
    /// Exact paths, plus every changed path's ancestor folders.
    pub paths: HashMap<PathBuf, FileState>,
    /// Folders reported whole (ignored or untracked): everything under them inherits.
    pub trees: HashMap<PathBuf, FileState>,
}

impl Status {
    /// State of `path` inside `root`: its own, else that of a folder reported whole above it.
    pub fn get(&self, root: &Path, path: &Path) -> Option<FileState> {
        if let Some(g) = self.paths.get(path) {
            return Some(*g);
        }
        path.ancestors().take_while(|a| a.starts_with(root)).find_map(|a| self.trees.get(a).copied())
    }
}

fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn head_name(gitdir: &Path) -> String {
    match read(&gitdir.join("HEAD")) {
        Some(h) => match h.strip_prefix("ref: refs/heads/") {
            Some(b) => b.to_string(),
            None => format!("@{}", &h[..h.len().min(7)]),
        },
        None => "?".into(),
    }
}

/// The repo's common git dir, if `dir` is a checkout root.
pub fn common_dir(dir: &Path) -> Option<PathBuf> {
    let dotgit = dir.join(".git");
    let md = fs::metadata(&dotgit).ok()?;
    if md.is_dir() {
        return fs::canonicalize(dotgit).ok();
    }
    let gd = PathBuf::from(read(&dotgit)?.strip_prefix("gitdir: ")?);
    let gd = if gd.is_absolute() { gd } else { dir.join(gd) };
    let common = match read(&gd.join("commondir")) {
        Some(c) => {
            let c = PathBuf::from(c);
            if c.is_absolute() { c } else { gd.join(c) }
        }
        None => gd,
    };
    fs::canonicalize(common).ok()
}

/// The checkout's worktrees, main first, then linked ones by branch; `None` if `dir` isn't a
/// checkout root. Reads `.git` files directly, no subprocess.
pub fn worktrees(dir: &Path) -> Option<Vec<Worktree>> {
    let common = common_dir(dir)?;
    let mut out = Vec::new();
    if common.file_name() == Some(".git".as_ref()) {
        if let Some(main) = common.parent() {
            out.push(Worktree { path: main.to_path_buf(), branch: head_name(&common), main: true });
        }
    }
    if let Ok(rd) = fs::read_dir(common.join("worktrees")) {
        let mut v: Vec<Worktree> = rd
            .flatten()
            .filter_map(|e| {
                let gd = e.path();
                let wt = PathBuf::from(read(&gd.join("gitdir"))?).parent()?.to_path_buf();
                wt.exists().then(|| Worktree { path: wt, branch: head_name(&gd), main: false })
            })
            .collect();
        v.sort_by(|a, b| a.branch.cmp(&b.branch));
        out.extend(v);
    }
    Some(out)
}

/// Paths whose change means the worktree list or a branch changed.
pub fn watch_paths(dir: &Path) -> Vec<PathBuf> {
    let Some(common) = common_dir(dir) else { return vec![] };
    // FETCH_HEAD: a fetch moves upstream refs (behind counts) without touching the dirs above.
    let mut v = vec![common.clone(), common.join("FETCH_HEAD")];
    let wts = common.join("worktrees");
    if let Ok(rd) = fs::read_dir(&wts) {
        v.extend(rd.flatten().map(|e| e.path()));
    }
    v.push(wts);
    v
}

fn slug(branch: &str) -> String {
    let mut s = String::new();
    for ch in branch.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            s.push(ch);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    s.trim_matches('-').to_string()
}

/// Run `git -C dir args…`: stdout, or the last line of stderr.
fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into())
    } else {
        let e = String::from_utf8_lossy(&o.stderr);
        Err(e.trim().lines().last().unwrap_or("git failed").to_string())
    }
}

/// Add a worktree for `branch` (created from HEAD if it doesn't exist) under
/// `~/.pmux/worktrees/<repo>/<branch-slug>`. Returns its path.
pub fn add(project: &Path, branch: &str) -> Result<PathBuf, String> {
    let repo = common_dir(project)
        .and_then(|c| c.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "repo".into());
    let dest = crate::config::home().join(".pmux/worktrees").join(repo).join(slug(branch));
    let d = dest.to_string_lossy().into_owned();
    let exists = run(project, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
    if exists {
        run(project, &["worktree", "add", &d, branch])?;
    } else {
        run(project, &["worktree", "add", "-b", branch, &d])?;
    }
    Ok(dest)
}

/// `git worktree remove` (with `--force`: even with uncommitted changes).
pub fn remove(project: &Path, wt: &Path, force: bool) -> Result<(), String> {
    let w = wt.to_string_lossy();
    let args: &[&str] = if force { &["worktree", "remove", "--force", &w] } else { &["worktree", "remove", &w] };
    run(project, args).map(|_| ())
}

/// Commits HEAD is behind its upstream; 0 without one. Read-only: writes nothing under .git.
pub fn behind(wt: &Path) -> u32 {
    run(wt, &["--no-optional-locks", "rev-list", "--count", "HEAD..@{u}"]).ok().and_then(|o| o.trim().parse().ok()).unwrap_or(0)
}

/// `git status` for the repo containing `root`, limited to `root` (blocking: call off the UI thread).
///
/// Runs with `--no-optional-locks`: a plain `git status` refreshes the index, which writes
/// `.git/index.lock` and touches the git dir. We watch that dir (`watch_paths`), and a change
/// there runs this again, so without the flag pmux loops on `git status` forever.
pub fn status(root: &Path) -> Status {
    let mut st = Status::default();
    let Ok(top) = run(root, &["rev-parse", "--show-toplevel"]) else { return st };
    let top = PathBuf::from(top.trim());
    let top = fs::canonicalize(&top).unwrap_or(top);
    let Ok(out) = run(root, &["--no-optional-locks", "status", "--porcelain=v1", "-z", "--ignored", "-unormal", "--", "."]) else { return st };
    let mut recs = out.split('\0');
    while let Some(r) = recs.next() {
        // "XY path"; a rename/copy is followed by its source path as a record of its own (which
        // may be as short as one byte, so records can't be filtered by length up front).
        if r.len() < 4 {
            continue;
        }
        let (x, y) = (r.as_bytes()[0], r.as_bytes()[1]);
        let rel = &r[3..];
        if x == b'R' || x == b'C' {
            recs.next(); // the rename's source path
        }
        let g = match (x, y) {
            (b'!', b'!') => FileState::Ignored,
            (b'?', b'?') => FileState::Added,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => FileState::Conflict,
            (b'D', _) | (_, b'D') => FileState::Deleted,
            (b'A', _) => FileState::Added,
            _ => FileState::Modified,
        };
        let path = top.join(rel.trim_end_matches('/'));
        if rel.ends_with('/') {
            st.trees.insert(path.clone(), g);
        } else {
            st.paths.insert(path.clone(), g);
        }
        if g == FileState::Ignored {
            continue;
        }
        for a in path.ancestors().skip(1).take_while(|a| a.starts_with(&top)) {
            let e = st.paths.entry(a.to_path_buf()).or_insert(g);
            *e = (*e).max(g);
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    /// The sidebar removes a worktree without `--force` first and asks only when git refuses
    /// with a message telling to use `--force` (`App::worktree_removed`): check git says so.
    #[test]
    fn unforced_remove_refuses_dirty_worktree_and_says_force() {
        let tmp = std::env::temp_dir().join(format!("pmux-git-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let (repo, wt) = (tmp.join("repo"), tmp.join("wt"));
        fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);
        sh(&repo, &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()]);
        fs::write(wt.join("new.txt"), "unsaved work").unwrap();
        let e = remove(&repo, &wt, false).unwrap_err();
        assert!(e.contains("--force"), "{e}");
        assert!(wt.join("new.txt").exists(), "nothing deleted");
        fs::remove_file(wt.join("new.txt")).unwrap();
        remove(&repo, &wt, false).unwrap();
        assert!(!wt.exists());
        fs::remove_dir_all(&tmp).unwrap();
    }
}
