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

/// Before deleting a merged worktree: if all git finds besides its commits is untracked
/// symlinks, unlink them (the link only, never what it points to). Agents link `node_modules`
/// in from the main checkout, and `node_modules/` in .gitignore matches folders, not links, so
/// git counts the link as an untracked file and refuses to remove the worktree. Anything else
/// uncommitted: leave it all, git refuses then and the worktree stays.
pub fn drop_untracked_links(wt: &Path) {
    let Ok(out) = read_only(wt, &["status", "--porcelain=v1", "-z", "-unormal"]) else { return };
    let paths: Vec<PathBuf> = out.split('\0').filter(|r| !r.is_empty()).map(|r| r.strip_prefix("?? ").map(|p| wt.join(p.trim_end_matches('/')))).collect::<Option<_>>().unwrap_or_default();
    if paths.iter().all(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())) {
        for p in paths {
            let _ = fs::remove_file(p);
        }
    }
}

/// `git branch -d` (with `force`: `-D`, for a branch merged by squash, which `-d` can't tell).
pub fn delete_branch(project: &Path, branch: &str, force: bool) -> Result<(), String> {
    run(project, &["branch", if force { "-D" } else { "-d" }, branch]).map(|_| ())
}

/// How a branch got into the default branch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Merge {
    /// Its tip is in the default branch's history (a merge commit or a fast-forward).
    Ancestor,
    /// One commit on the default branch makes the same change as the whole branch.
    Squash,
    /// GitHub says a PR from it was merged at exactly its tip ("Squash and merge", rebase).
    Pr,
}

impl Merge {
    /// `git branch -d` only knows about ancestry: the other ways need `-D`.
    pub fn force_delete(self) -> bool {
        self != Merge::Ancestor
    }
}

/// `run` with `--no-optional-locks`: these reads must not write under .git (that's watched).
fn read_only(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut a = vec!["--no-optional-locks"];
    a.extend_from_slice(args);
    run(dir, &a)
}

/// A project's branches and `origin`'s, read with one `git for-each-ref`: each ref's commit,
/// and how far each local branch is behind its upstream. One process per project, where a
/// `rev-parse` per ref and a `rev-list` per worktree took several per worktree.
#[derive(Default)]
pub struct Refs {
    commits: HashMap<String, String>,
    /// Local branches behind their upstream (only nonzero ones).
    behind: HashMap<String, u32>,
}

/// Read `project`'s refs (blocking: call off the UI thread). Empty when git fails.
pub fn refs(project: &Path) -> Refs {
    let fmt = "--format=%(refname)%00%(objectname)%00%(upstream:track,nobracket)";
    Refs::parse(&read_only(project, &["for-each-ref", fmt, "refs/heads", "refs/remotes/origin"]).unwrap_or_default())
}

impl Refs {
    /// `for-each-ref` lines: ref, commit and upstream track ("ahead 1, behind 3", "behind 3",
    /// "gone" or nothing), NUL-separated.
    fn parse(out: &str) -> Refs {
        let mut r = Refs::default();
        for l in out.lines() {
            let mut f = l.split('\0');
            let (Some(name), Some(commit)) = (f.next(), f.next()) else { continue };
            let behind: u32 = f.next().unwrap_or("").split(", ").find_map(|t| t.strip_prefix("behind ")?.parse().ok()).unwrap_or(0);
            if let Some(b) = name.strip_prefix("refs/heads/").filter(|_| behind > 0) {
                r.behind.insert(b.to_string(), behind);
            }
            r.commits.insert(name.to_string(), commit.to_string());
        }
        r
    }

    /// The default branch to compare against: local `main`/`master`, and its `origin` copy (a
    /// fetch after a merge on GitHub moves that one first). Pairs of (ref, commit).
    fn bases(&self) -> Vec<(String, String)> {
        for b in ["main", "master"] {
            let v: Vec<(String, String)> = [format!("refs/heads/{b}"), format!("refs/remotes/origin/{b}")]
                .into_iter()
                .filter_map(|r| self.commits.get(&r).map(|c| (r.clone(), c.clone())))
                .collect();
            if !v.is_empty() {
                return v;
            }
        }
        vec![]
    }

    /// What the merge verdict for `branch` depends on: its tip and the default branch's commits.
    /// Same key, same verdict, so a verdict is only worked out again when this changes.
    pub fn merge_key(&self, branch: &str) -> Option<String> {
        let mut k = self.commits.get(&format!("refs/heads/{branch}"))?.clone();
        for (_, c) in self.bases() {
            k.push(' ');
            k.push_str(&c);
        }
        Some(k)
    }

    /// Commits `branch` is behind its upstream; 0 without one.
    pub fn behind(&self, branch: &str) -> u32 {
        self.behind.get(branch).copied().unwrap_or(0)
    }
}

/// `git patch-id --stable` of a diff or log: the patch ids, one per commit.
fn patch_ids(dir: &Path, diff: &str) -> Vec<String> {
    use std::io::Write;
    use std::process::Stdio;
    let Ok(mut c) = Command::new("git").arg("-C").arg(dir).args(["patch-id", "--stable"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() else {
        return vec![];
    };
    let mut stdin = c.stdin.take().expect("piped");
    let input = diff.to_string();
    // Feed it from another thread: a big diff fills the pipes both ways otherwise.
    let feed = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = c.wait_with_output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let _ = feed.join();
    out.lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()
}

/// Whether the branch made its tip itself, from its reflog (`%H %gs` lines, newest first): the
/// newest entry that set it to `tip` committed on the branch. Not when it only took commits
/// that were there already: created from main, fast-forwarded or reset to it, or rebased onto
/// it with nothing of its own (then its tip is main's, and in main's history, but not merged
/// work). Anything else counts as not its own: better keep a worktree than lose one.
fn own_tip(reflog: &str, tip: &str) -> bool {
    let Some(msg) = reflog.lines().find_map(|l| l.strip_prefix(tip)).map(str::trim) else { return false };
    if let Some(rest) = msg.strip_prefix("rebase (finish):") {
        return rest.rsplit_once(" onto ").is_some_and(|(_, onto)| onto.trim() != tip);
    }
    ["commit", "cherry-pick", "revert", "merge"].iter().any(|p| msg.starts_with(p)) && !msg.ends_with("Fast-forward")
}

/// Newest commits on the default branch searched for a squash of a branch.
const SQUASH_SEARCH: &str = "300";

/// Whether linked worktree branch `branch` of `project` is merged into the default branch,
/// and how (blocking, may ask GitHub: call off the UI thread). `None` for a branch that has no
/// commits of its own yet: a fresh worktree's tip *is* in main, but it isn't done, it's new.
pub fn merged(project: &Path, wt: &Path, branch: &str) -> Option<Merge> {
    let head = format!("refs/heads/{branch}");
    let tip = read_only(project, &["rev-parse", "--verify", "--quiet", &head]).ok()?.trim().to_string();
    let log = read_only(project, &["reflog", "show", "--format=%H %gs", &head]).ok()?;
    if !own_tip(&log, &tip) {
        return None;
    }
    let bases = refs(project).bases();
    if bases.iter().any(|(r, _)| read_only(project, &["merge-base", "--is-ancestor", &tip, r]).is_ok()) {
        return Some(Merge::Ancestor);
    }
    for (r, _) in &bases {
        let Ok(mb) = read_only(project, &["merge-base", &tip, r]) else { continue };
        let mb = mb.trim();
        let Ok(diff) = read_only(project, &["diff", "--no-color", "--no-ext-diff", mb, &tip]) else { continue };
        let Some(want) = patch_ids(project, &diff).into_iter().next() else { continue };
        let range = format!("{mb}..{r}");
        let Ok(log) = read_only(project, &["log", "-p", "--no-merges", "--no-color", "--no-ext-diff", "-n", SQUASH_SEARCH, &range]) else { continue };
        if patch_ids(project, &log).contains(&want) {
            return Some(Merge::Squash);
        }
    }
    // A squash that git can't match (main changed the same lines meanwhile): ask GitHub, for
    // a branch that was pushed. Only a PR merged at exactly our tip counts: no lost commits.
    read_only(project, &["rev-parse", "--verify", "--quiet", &format!("{branch}@{{upstream}}")]).ok()?;
    let o = Command::new(gh()?).current_dir(wt).args(["pr", "list", "--head", branch, "--state", "merged", "--json", "headRefOid", "--jq", ".[].headRefOid"]).output().ok()?;
    (o.status.success() && String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == tip)).then_some(Merge::Pr)
}

/// The GitHub CLI. An app opened from the Finder gets a bare PATH, without Homebrew's.
fn gh() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&path).chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.map(|d| d.join("gh")).find(|p| p.is_file())
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

    fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
        fs::write(dir.join(file), text).unwrap();
        sh(dir, &["add", file]);
        sh(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", msg]);
    }

    /// A repo on `main` with one commit, and a linked worktree on new branch `name`.
    fn repo_with_branch(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("pmux-merged-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let (repo, wt) = (tmp.join("repo"), tmp.join("wt"));
        fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        commit(&repo, "a.txt", "one\n", "init");
        sh(&repo, &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()]);
        (tmp, repo, wt)
    }

    #[test]
    fn refs_give_merge_keys_and_behind_counts() {
        let out = "refs/heads/main\0aaa\0behind 2\nrefs/heads/f/x\0bbb\0ahead 1, behind 3\nrefs/heads/g\0ccc\0gone\nrefs/remotes/origin/main\0ddd\0\n";
        let r = Refs::parse(out);
        assert_eq!(r.merge_key("f/x").as_deref(), Some("bbb aaa ddd"));
        assert_eq!(r.merge_key("nope"), None);
        assert_eq!((r.behind("main"), r.behind("f/x"), r.behind("g"), r.behind("nope")), (2, 3, 0, 0));
        let master = Refs::parse("refs/heads/master\0m\0\nrefs/heads/b\0t\0\n");
        assert_eq!(master.merge_key("b").as_deref(), Some("t m"));
    }

    /// Real git: one `for-each-ref` reads what `rev-parse` and `rev-list` did.
    #[test]
    fn refs_read_from_git() {
        let (tmp, repo, wt) = repo_with_branch("refs");
        sh(&wt, &["branch", "-q", "--set-upstream-to=main"]);
        commit(&repo, "b.txt", "two\n", "on main");
        let r = refs(&repo);
        let main = run(&repo, &["rev-parse", "main"]).unwrap();
        let tip = run(&repo, &["rev-parse", "refs"]).unwrap();
        assert_eq!(r.merge_key("refs"), Some(format!("{} {}", tip.trim(), main.trim())));
        assert_eq!((r.behind("refs"), r.behind("main")), (1, 0));
        let _ = fs::remove_dir_all(tmp);
    }

    /// A fresh worktree's tip is in main already, but it's new work, not merged work.
    #[test]
    fn fresh_and_unmerged_branches_are_not_merged() {
        let (tmp, repo, wt) = repo_with_branch("fresh");
        assert_eq!(merged(&repo, &wt, "fresh"), None, "fresh");
        // Rebased onto a newer main with nothing of its own: its tip is main's, still new.
        commit(&repo, "c.txt", "main\n", "main moves on");
        sh(&wt, &["rebase", "-q", "main"]);
        assert_eq!(merged(&repo, &wt, "fresh"), None, "rebased, no work");
        commit(&wt, "b.txt", "work\n", "work");
        assert_eq!(merged(&repo, &wt, "fresh"), None, "unmerged");
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// Links go only when nothing else is uncommitted, and their targets stay.
    #[test]
    fn untracked_links_are_dropped_only_alone() {
        let (tmp, repo, wt) = repo_with_branch("ln");
        let target = tmp.join("deps");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("dep.js"), "x").unwrap();
        std::os::unix::fs::symlink(&target, wt.join("node_modules")).unwrap();
        fs::write(wt.join("notes.txt"), "unsaved").unwrap();
        drop_untracked_links(&wt);
        assert!(wt.join("node_modules").exists(), "kept beside a real untracked file");
        fs::remove_file(wt.join("notes.txt")).unwrap();
        drop_untracked_links(&wt);
        assert!(fs::symlink_metadata(wt.join("node_modules")).is_err(), "link dropped");
        assert!(target.join("dep.js").exists(), "target untouched");
        remove(&repo, &wt, false).unwrap();
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// Own work, rebased onto main, then fast-forwarded into it.
    #[test]
    fn rebased_work_merged_counts() {
        let (tmp, repo, wt) = repo_with_branch("rb");
        commit(&wt, "b.txt", "work\n", "work");
        commit(&repo, "c.txt", "main\n", "main moves on");
        sh(&wt, &["-c", "user.name=t", "-c", "user.email=t@t", "rebase", "-q", "main"]);
        assert_eq!(merged(&repo, &wt, "rb"), None, "not merged yet");
        sh(&repo, &["merge", "-q", "--ff-only", "rb"]);
        assert_eq!(merged(&repo, &wt, "rb"), Some(Merge::Ancestor));
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn merge_and_fast_forward_count_as_ancestor() {
        let (tmp, repo, wt) = repo_with_branch("ff");
        commit(&wt, "b.txt", "work\n", "work");
        sh(&repo, &["merge", "-q", "--ff-only", "ff"]);
        assert_eq!(merged(&repo, &wt, "ff"), Some(Merge::Ancestor));
        fs::remove_dir_all(&tmp).unwrap();
    }

    /// GitHub's "Squash and merge": one new commit on main with the branch's whole change,
    /// also when main moved on meanwhile.
    #[test]
    fn squash_merge_is_found_by_patch_id() {
        let (tmp, repo, wt) = repo_with_branch("sq");
        commit(&wt, "b.txt", "first\n", "one");
        commit(&wt, "b.txt", "first\nsecond\n", "two");
        commit(&repo, "c.txt", "elsewhere\n", "main moves on");
        sh(&repo, &["merge", "-q", "--squash", "sq"]);
        assert_eq!(merged(&repo, &wt, "sq"), None, "squash not committed yet");
        sh(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "squashed"]);
        assert_eq!(merged(&repo, &wt, "sq"), Some(Merge::Squash));
        // More work after the squash: not done any more.
        commit(&wt, "b.txt", "first\nsecond\nthird\n", "three");
        assert_eq!(merged(&repo, &wt, "sq"), None, "work after the squash");
        fs::remove_dir_all(&tmp).unwrap();
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
