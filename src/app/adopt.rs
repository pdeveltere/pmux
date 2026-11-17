//! `pmux import herdr` in the window: projects for the terminals' folders, then one tab per
//! Herdr tab with a shell per terminal (and its command typed in).
use super::App;
use crate::api::Reply;
use crate::import::Plan;
use std::path::{Path, PathBuf};

/// Workspace that projects new to pmux land in.
const IMPORT_WS: &str = "herdr";

impl App {
    /// The deepest project or worktree folder containing `dir`.
    fn view_for(&self, dir: &Path) -> Option<PathBuf> {
        self.projects().flat_map(|p| p.view_paths()).filter(|v| dir.starts_with(v)).max_by_key(|v| v.as_os_str().len())
    }

    /// The repo `dir` belongs to (its main checkout, for a linked worktree), else `dir` itself.
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

    pub(super) fn api_import(&mut self, json: &str) -> Reply {
        let plan: Plan = match serde_json::from_str(json) {
            Ok(p) => p,
            Err(e) => return Reply::err(format!("bad import: {e}")),
        };
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
                    if let Err(e) = self.add_workspace(IMPORT_WS) {
                        return Reply::err(e);
                    }
                    self.workspaces.len() - 1
                }
            };
            if let Ok(name) = self.add_project(w, &Self::repo_of(&cwd).display().to_string()) {
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
