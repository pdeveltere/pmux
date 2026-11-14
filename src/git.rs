//! Worktree discovery by reading .git files directly: no `git` subprocess.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub main: bool,
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

fn run(project: &Path, args: &[&str]) -> Result<String, String> {
    let o = Command::new("git").arg("-C").arg(project).args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into())
    } else {
        let e = String::from_utf8_lossy(&o.stderr);
        Err(e.trim().lines().last().unwrap_or("git failed").to_string())
    }
}

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

pub fn remove(project: &Path, wt: &Path, force: bool) -> Result<(), String> {
    let w = wt.to_string_lossy();
    let args: &[&str] = if force { &["worktree", "remove", "--force", &w] } else { &["worktree", "remove", &w] };
    run(project, args).map(|_| ())
}

/// Uncommitted or untracked changes in a worktree (what makes `git worktree remove` refuse).
pub fn dirty(wt: &Path) -> bool {
    run(wt, &["status", "--porcelain"]).map(|o| !o.trim().is_empty()).unwrap_or(false)
}

/// Commits HEAD is behind its upstream; 0 without one. Read-only: writes nothing under .git.
pub fn behind(wt: &Path) -> u32 {
    run(wt, &["--no-optional-locks", "rev-list", "--count", "HEAD..@{u}"]).ok().and_then(|o| o.trim().parse().ok()).unwrap_or(0)
}
