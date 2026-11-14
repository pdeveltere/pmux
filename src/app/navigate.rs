//! Commands on the current view: new panes and tabs, moving focus, switching worktrees.
use super::{App, State, View};
use std::path::{Path, PathBuf};

impl App {
    /// New pane in the current view, shell in `cwd` (default: the view), beside the others or in a
    /// new tab; focused.
    pub(super) fn spawn_here(&mut self, cwd: Option<&Path>, new_tab: bool) -> Option<u32> {
        let view = self.current.clone()?;
        match self.spawn_pane(&view, cwd.unwrap_or(&view), new_tab) {
            Ok(id) => {
                self.focus_panes();
                Some(id)
            }
            Err(e) => {
                self.say(format!("spawn failed: {e}"));
                None
            }
        }
    }

    pub(super) fn split(&mut self) {
        self.spawn_here(None, false);
    }

    pub(super) fn new_tab(&mut self) {
        self.spawn_here(None, true);
    }

    /// Show tab `i` of the current view (wrapping when `rel`, relative to the shown one).
    pub(super) fn select_tab(&mut self, i: isize, rel: bool) {
        let Some(v) = self.current_view_mut() else { return };
        let n = v.tabs.len() as isize;
        if n == 0 {
            return;
        }
        let t = if rel { (v.tab as isize + i).rem_euclid(n) } else { i };
        if (0..n).contains(&t) {
            v.tab = t as usize;
        }
        self.focus_panes();
    }

    /// Close every pane in tab `i` of the current view; the tab disappears once they exit.
    pub(super) fn close_tab(&mut self, i: usize) {
        let ids = self.current_view().and_then(|v| v.tabs.get(i)).map(|t| t.panes.clone()).unwrap_or_default();
        self.kill_panes(ids);
    }

    pub(super) fn close_focused(&mut self) {
        if let Some(id) = self.focused() {
            self.kill_pane(id);
        }
    }

    /// Clear only this pane: wipe its screen + scrollback, then let the app redraw (Ctrl-L).
    pub(super) fn clear_focused(&mut self) {
        if let Some(id) = self.focused() {
            if let Some(p) = self.panes.get_mut(&id) {
                p.vt.clear();
            }
            self.write_pane(id, &[0x0c]);
        }
    }

    /// Jump to the lowest-numbered pane waiting for the user.
    pub(super) fn next_attention(&mut self) {
        let Some((id, v)) = self.panes.values().filter(|p| p.state() == Some(State::Waiting)).map(|p| (p.id, p.view.clone())).min() else { return };
        self.open_view(v.clone());
        if let Some(view) = self.views.get_mut(&v) {
            view.reveal(id);
        }
        self.see_focused();
    }

    /// Focus the next/previous pane in the shown tab.
    pub(super) fn move_focus(&mut self, d: isize) {
        if let Some(t) = self.current_view_mut().and_then(View::cur_mut) {
            let n = t.panes.len() as isize;
            if n > 0 {
                t.focus = (t.focus as isize + d).rem_euclid(n) as usize;
            }
        }
        self.focus_panes();
    }

    /// Open the next/previous worktree in sidebar order.
    pub(super) fn step_view(&mut self, d: isize) {
        let views: Vec<PathBuf> = self.rows().into_iter().filter_map(|r| self.row_view(r)).collect();
        if views.is_empty() {
            return;
        }
        let i = self.current.as_ref().and_then(|c| views.iter().position(|v| v == c)).map_or(-1, |i| i as isize);
        let n = views.len() as isize;
        self.open_view(views[(i + d).rem_euclid(n) as usize].clone());
    }
}
