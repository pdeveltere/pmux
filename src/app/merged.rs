//! Merged worktrees go away by themselves. When a linked worktree's branch is in the default
//! branch (merged, fast-forwarded, squashed, or a PR GitHub merged at its tip: `git::merged`),
//! pmux deletes the worktree the safe way (`git worktree remove` without `--force`) and then
//! its branch. Not while an agent in it is working or needs you: that waits until it's idle.
//! With uncommitted or untracked files git refuses, and the worktree stays, marked "merged"
//! (untracked symlinks alone don't count: `git::drop_untracked_links`).
use super::{App, Ev, Removal, State};
use crate::git;
use std::path::{Path, PathBuf};

/// A linked worktree whose branch is merged, waiting to be deleted (or kept).
pub struct Merged {
    pub proj: PathBuf,
    pub branch: String,
    pub how: git::Merge,
    /// Deleting it failed (uncommitted files): it stays until the user deletes it.
    pub kept: bool,
}

/// What an automatic removal does after `git worktree remove`.
pub struct Auto {
    pub branch: String,
    pub how: git::Merge,
    /// Set off the UI thread when the branch couldn't be deleted.
    pub branch_err: Option<String>,
}

impl App {
    /// Off the UI thread: for each linked worktree in `views` whose branch or default branch
    /// moved since its last check, work out whether it's merged; `Ev::Merged` reports each.
    pub(super) fn check_merged(&self, views: &[PathBuf]) {
        let todo: Vec<(PathBuf, PathBuf, String, Option<String>)> = self
            .projects()
            .flat_map(|p| p.worktrees.iter().flatten().map(move |w| (p, w)))
            .filter(|(_, w)| !w.main && !w.branch.starts_with('@') && !super::model::is_default_branch(&w.branch) && views.contains(&w.path))
            .map(|(p, w)| (p.path.clone(), w.path.clone(), w.branch.clone(), self.merge_keys.get(&w.path).cloned()))
            .collect();
        if todo.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for (proj, wt, branch, seen) in todo {
                let Some(key) = git::merge_key(&proj, &branch) else { continue };
                if seen.as_ref() == Some(&key) {
                    continue;
                }
                let how = git::merged(&proj, &wt, &branch);
                if tx.send_event(Ev::Merged { proj, wt, branch, key, how }).is_err() {
                    return;
                }
            }
        });
    }

    pub(super) fn merged_checked(&mut self, proj: PathBuf, wt: PathBuf, branch: String, key: String, how: Option<git::Merge>) {
        // Gone or switched branch meanwhile: the verdict is about something else.
        let current = self.projects().flat_map(|p| p.worktrees.iter().flatten()).any(|w| w.path == wt && w.branch == branch);
        if !current {
            return;
        }
        self.merge_keys.insert(wt.clone(), key);
        match how {
            None => {
                self.merged.remove(&wt);
            }
            Some(how) => {
                // Once git refused (uncommitted files), don't retry on every new commit on main.
                let kept = self.merged.get(&wt).is_some_and(|m| m.kept);
                self.merged.insert(wt, Merged { proj, branch, how, kept });
                self.remove_merged();
            }
        }
    }

    /// Start deleting each merged worktree no agent in it is busy in.
    pub(super) fn remove_merged(&mut self) {
        let ready: Vec<(PathBuf, PathBuf, String, git::Merge)> = self
            .merged
            .iter()
            .filter(|(wt, m)| !m.kept && !self.removing.contains(*wt) && !self.busy_in(wt))
            .map(|(wt, m)| (wt.clone(), m.proj.clone(), m.branch.clone(), m.how))
            .collect();
        for (wt, proj, branch, how) in ready {
            let name = format!("merged worktree {branch}");
            let auto = Some(Auto { branch, how, branch_err: None });
            self.start_removal(Removal { proj, wt, name, force: false, reply: None, auto });
        }
    }

    /// Whether an agent in worktree `wt` is working or waiting for an answer, or an editor in
    /// it has unsaved changes (git can't see those).
    fn busy_in(&self, wt: &Path) -> bool {
        self.panes_in(wt).iter().filter_map(|id| self.panes.get(id)).any(|p| {
            p.editor.as_ref().is_some_and(|e| e.modified()) || p.state().is_some_and(|s| matches!(s, State::Working | State::Waiting | State::Error))
        })
    }

    /// An automatic removal finished: `Ok` deleted it (and its branch, unless `branch_err`).
    pub(super) fn merged_removed(&mut self, wt: &Path, name: &str, auto: &Auto, res: &Result<(), String>) {
        match res {
            Ok(()) => {
                self.merged.remove(wt);
                self.merge_keys.remove(wt);
                match &auto.branch_err {
                    None => self.say(format!("deleted {name} and its branch")),
                    Some(e) => self.say(format!("deleted {name}; kept branch {}: {e}", auto.branch)),
                }
            }
            Err(e) => {
                if let Some(m) = self.merged.get_mut(wt) {
                    m.kept = true;
                }
                if e.contains("--force") {
                    self.say(format!("{name} has uncommitted or untracked files: kept it"));
                } else {
                    self.say(format!("couldn't delete {name}: {e}"));
                }
            }
        }
    }
}
