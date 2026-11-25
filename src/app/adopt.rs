//! `pmux import herdr` in the window: projects for the terminals' folders, then one tab per
//! Herdr tab with a shell per terminal (and its command typed in).
use super::{App, Ev};
use crate::api::Reply;
use crate::import::Plan;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// Workspace that projects new to pmux land in.
const IMPORT_WS: &str = "herdr";

impl App {
    /// The deepest project or worktree folder containing `dir`.
    fn view_for(&self, dir: &Path) -> Option<PathBuf> {
        self.projects().flat_map(|p| p.view_paths()).filter(|v| dir.starts_with(v)).max_by_key(|v| v.as_os_str().len())
    }

    /// The repo `dir` belongs to (its main checkout, for a linked worktree), else `dir` itself.
    /// Runs git: call off the UI thread.
    fn repo_of(dir: &Path) -> PathBuf {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).output();
        match out {
            Ok(o) if o.status.success() => {
                let common = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
                match common.file_name().and_then(|n| n.to_str()) {
                    Some(".git") => common.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf()),
                    _ => dir.to_path_buf(),
                }
            }
            _ => dir.to_path_buf(),
        }
    }

    /// `pmux import`: look up each folder's repo on a thread (one `git` per folder), then
    /// `Ev::ImportReady` opens the terminals; the caller gets its reply then.
    pub(super) fn start_import(&self, json: String, rtx: Sender<Reply>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let plan: Plan = match serde_json::from_str(&json) {
                Ok(p) => p,
                Err(e) => return drop(rtx.send(Reply::err(format!("bad import: {e}")))),
            };
            let mut repos = HashMap::new();
            for it in &plan.items {
                let cwd = std::fs::canonicalize(&it.cwd).unwrap_or_else(|_| it.cwd.clone());
                if cwd.is_dir() && !repos.contains_key(&cwd) {
                    repos.insert(cwd.clone(), Self::repo_of(&cwd));
                }
            }
            let _ = tx.send_event(Ev::ImportReady(plan, repos, rtx));
        });
    }

    /// Open an import's terminals; `repos` maps each existing folder to its repo.
    pub(super) fn api_import(&mut self, plan: Plan, repos: &HashMap<PathBuf, PathBuf>) -> Reply {
        // Projects first, so every folder has a view.
        let mut added = Vec::new();
        for it in &plan.items {
            let cwd = std::fs::canonicalize(&it.cwd).unwrap_or_else(|_| it.cwd.clone());
            if self.view_for(&cwd).is_some() || !cwd.is_dir() {
                continue;
            }
            let w = match self.workspaces.iter().position(|w| w.name == IMPORT_WS) {
                Some(w) => w,
                None => {
                    // At ~, named for Herdr.
                    let home = std::fs::canonicalize(crate::config::home()).unwrap_or_else(|_| crate::config::home());
                    self.workspaces.push(super::Workspace { name: IMPORT_WS.into(), path: home, projects: vec![] });
                    self.workspaces.len() - 1
                }
            };
            let repo = repos.get(&cwd).cloned().unwrap_or_else(|| cwd.clone());
            if let Ok(name) = self.add_project(w, &repo.display().to_string()) {
                added.push(name);
            }
        }
        let (mut opened, mut tabs, mut skipped) = (0, 0, Vec::new());
        let mut group: Option<(&str, PathBuf)> = None;
        for it in &plan.items {
            let cwd = std::fs::canonicalize(&it.cwd).unwrap_or_else(|_| it.cwd.clone());
            let Some(view) = self.view_for(&cwd) else {
                skipped.push(it.cwd.display().to_string());
                continue;
            };
            // Same Herdr tab and same view: split the tab just made; else a new tab.
            let same = group.as_ref().is_some_and(|(g, v)| *g == it.group && *v == view);
            if !same {
                self.views.entry(view.clone()).or_default();
            }
            let id = match self.spawn_pane(&view, &cwd, !same) {
                Ok(id) => id,
                Err(e) => return Reply::err(format!("opened {opened}, then: {e}")),
            };
            if !same {
                tabs += 1;
            }
            group = Some((&it.group, view));
            opened += 1;
            if let Some(cmd) = &it.cmd {
                let line = if plan.run { format!("{cmd}\r") } else { cmd.clone() };
                self.write_pane(id, line.as_bytes());
            }
            if let (Some(t), Some(p)) = (&it.title, self.panes.get_mut(&id)) {
                p.title = Some(t.clone());
            }
        }
        let mut s = format!("opened {opened} terminals in {tabs} tabs");
        if !added.is_empty() {
            s += &format!("; added to workspace {IMPORT_WS}: {}", added.join(", "));
        }
        if !plan.run {
            s += "\ncommands are typed at each prompt: press Enter in a tab to start it (or import with --run)";
        }
        if !skipped.is_empty() {
            s += &format!("\nskipped (folder gone): {}", skipped.join(", "));
        }
        Reply::ok(s)
    }
}
