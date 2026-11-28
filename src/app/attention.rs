//! Telling you when an agent needs you: a macOS notification when one starts waiting for input,
//! or finishes, while you aren't looking at it (pmux in the background, or another pane on
//! screen), and the Dock badge counting agents that wait. See `crate::notify`.
use super::{App, State};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct Attention {
    /// Each agent's state when last checked (absent: not seen yet, so no change to report).
    last: HashMap<u32, Option<State>>,
    /// Panes with a notification up.
    posted: HashSet<u32>,
    /// The number on the Dock badge.
    badge: usize,
}

impl App {
    /// Compare every agent's state with the last check and notify about the changes that need
    /// you. Cheap: runs whenever the window is about to sleep.
    pub fn check_attention(&mut self) {
        let shown: HashSet<u32> = match (&self.current, self.window_focused) {
            (Some(v), true) => self.tab_panes(v).iter().copied().collect(),
            _ => HashSet::new(),
        };
        let mut waiting = 0;
        let mut post = Vec::new();
        let mut gone = Vec::new();
        for p in self.panes.values() {
            if p.agent.is_none() && p.state.is_none() {
                continue;
            }
            let s = p.state();
            waiting += usize::from(s == Some(State::Waiting));
            let prev = self.attention.last.insert(p.id, s);
            if shown.contains(&p.id) {
                if self.attention.posted.remove(&p.id) {
                    gone.push(p.id);
                }
                continue;
            }
            let Some(prev) = prev else { continue };
            if prev == s {
                continue;
            }
            match s {
                Some(State::Waiting) => post.push((p.id, "needs you")),
                Some(State::Done | State::Idle) if prev == Some(State::Working) => post.push((p.id, "is done")),
                Some(State::Error) => post.push((p.id, "hit an error")),
                // Working again: what was posted is stale.
                _ => {
                    if self.attention.posted.remove(&p.id) {
                        gone.push(p.id);
                    }
                }
            }
        }
        self.attention.last.retain(|id, _| self.panes.contains_key(id));
        for id in self.attention.posted.clone() {
            if !self.panes.contains_key(&id) {
                self.attention.posted.remove(&id);
                gone.push(id);
            }
        }
        #[cfg(target_os = "macos")]
        {
            for id in gone {
                crate::notify::remove(id);
            }
            if self.settings.notifications {
                for (id, what) in post {
                    let Some(p) = self.panes.get(&id) else { continue };
                    let (_, project, branch) = self.names(&p.view);
                    let title = if project == branch { project } else { format!("{project} · {branch}") };
                    crate::notify::post(id, &title, &format!("{} {what}", p.label()));
                    self.attention.posted.insert(id);
                }
            }
            if waiting != self.attention.badge {
                self.attention.badge = waiting;
                crate::notify::badge(waiting);
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (gone, post, waiting);
    }
}
