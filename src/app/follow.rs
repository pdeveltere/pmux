//! Tabs follow their terminal into another worktree (c11's "the folder is the truth", applied to
//! pmux's tree): when a pane's shell `cd`s, or its agent moves (`cd`, `EnterWorktree`), into
//! another worktree, of its own project or any other (or a workspace's own folder), its tab moves there. A pane split beside others moves
//! out into a tab of its own.
use super::{App, Tab};
use crate::hold::Moved;
use std::path::{Path, PathBuf};

impl App {
    /// Pane `id` works in `dir` now.
    pub(super) fn follow_cwd(&mut self, id: u32, dir: &Path) {
        let Some(p) = self.panes.get(&id).filter(|p| p.is_term()) else { return };
        let Some(to) = self.follow_target(&p.view, dir) else { return };
        self.move_pane(id, to);
    }

    /// The view `dir` is in (the deepest, when they nest), if it isn't `from`.
    fn follow_target(&self, from: &Path, dir: &Path) -> Option<PathBuf> {
        let views = self.workspaces.iter().flat_map(super::model::Workspace::view_paths);
        let to = views.filter(|v| dir.starts_with(v)).max_by_key(|v| v.as_os_str().len())?;
        (to != from).then_some(to)
    }

    /// Move pane `id` to a new tab of view `to` (its whole tab, when it was alone in it). If it was
    /// the focused pane, the window goes along.
    fn move_pane(&mut self, id: u32, to: PathBuf) {
        let followed = self.focused() == Some(id);
        let Some(p) = self.panes.get_mut(&id) else { return };
        let from = std::mem::replace(&mut p.view, to.clone());
        if let Some(v) = self.views.get_mut(&from) {
            v.remove(id);
        }
        let v = self.views.entry(to.clone()).or_default();
        v.tabs.push(Tab { panes: vec![id], focus: 0 });
        if followed {
            v.tab = v.tabs.len() - 1;
        }
        let (ws, proj, wt) = self.names(&to);
        let env =
            vec![("PMUX_WORKSPACE".into(), ws), ("PMUX_PROJECT".into(), proj), ("PMUX_WORKTREE".into(), wt.clone())];
        if let Some(l) = &self.hold {
            l.moved(id, &Moved { view: to.clone(), env });
        }
        if followed {
            self.open_view(to);
        } else {
            self.say(format!("%{id} moved to {wt}"));
        }
        self.apply_layout();
        self.dirty = true;
    }
}
