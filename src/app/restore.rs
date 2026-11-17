//! Attaching to the background pmux: rebuild panes and tabs from the terminals it holds, and keep
//! it up to date with the layout so the next window can do the same.
use super::{App, Ev, Pane, State, Tab, View};
use crate::hold::{self, Link, Meta};
use crate::term::Vt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;

/// The window's layout, stored in the background pmux (terminal panes only).
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    current: Option<PathBuf>,
    views: Vec<SavedView>,
}

#[derive(Serialize, Deserialize)]
struct SavedView {
    path: PathBuf,
    tab: usize,
    tabs: Vec<SavedTab>,
}

#[derive(Serialize, Deserialize)]
struct SavedTab {
    panes: Vec<u32>,
    focus: usize,
}


impl App {
    /// The link to the background pmux, attaching (and starting it) if there's none.
    pub(super) fn link(&mut self) -> Result<Link, String> {
        if let Some(l) = &self.hold {
            return Ok(l.clone());
        }
        self.attach()?;
        self.hold.clone().ok_or_else(|| "background pmux unavailable".into())
    }

    /// Attach to the background pmux and adopt every terminal it holds. Returns the view that was
    /// on screen last time, if any.
    pub fn attach(&mut self) -> Result<Option<PathBuf>, String> {
        let (link, w, screens, r) = Link::connect()?;
        let tx = self.tx.clone();
        hold::pump(r, move |m| tx.send_event(Ev::Hold(m)).is_ok());
        let saved: Saved = serde_json::from_str(&w.layout).unwrap_or_default();
        let mut placed = std::collections::HashSet::new();
        for t in &w.terms {
            let (cols, rows) = (t.cols as usize, t.rows as usize);
            let mut p = Pane::new(t.id, &t.view, Vt::new(cols, rows), Some(link.clone()), None, (cols, rows));
            p.set_agent(t.agent.as_deref());
            p.set_screen(t.screen.as_deref().and_then(crate::detect::Screen::parse));
            p.title = t.meta.title.clone();
            p.state = t.meta.state.as_deref().and_then(State::parse);
            p.state_at = Instant::now();
            p.unseen = t.worked && p.agent.is_some();
            self.meta_last.insert(t.id, t.meta.clone());
            self.panes.insert(t.id, p);
        }
        self.next_id = self.next_id.max(w.next_id);
        for (id, b) in screens {
            if let Some(p) = self.panes.get_mut(&id) {
                // Rebuilding the screen isn't output: don't count it as the agent working.
                for e in p.vt.feed(&b) {
                    if let alacritty_terminal::event::Event::Title(t) = e {
                        p.osc_title = Some(t);
                    }
                }
            }
        }
        for sv in saved.views {
            let mut v = View::default();
            for st in sv.tabs {
                let panes: Vec<u32> =
                    st.panes.into_iter().filter(|id| self.panes.get(id).is_some_and(|p| p.view == sv.path) && placed.insert(*id)).collect();
                if !panes.is_empty() {
                    v.tabs.push(Tab { focus: st.focus.min(panes.len() - 1), panes });
                }
            }
            if !v.tabs.is_empty() {
                v.tab = sv.tab.min(v.tabs.len() - 1);
                self.views.insert(sv.path, v);
            }
        }
        // Terminals the layout doesn't know (opened after it was last saved): a tab each.
        let mut rest: Vec<u32> = w.terms.iter().map(|t| t.id).filter(|id| !placed.contains(id)).collect();
        rest.sort();
        for id in rest {
            let view = self.panes[&id].view.clone();
            let v = self.views.entry(view).or_default();
            v.tabs.push(Tab { panes: vec![id], focus: 0 });
        }
        self.hold = Some(link);
        // Terminals that moved to another worktree while no window was open.
        for t in &w.terms {
            if let Some(dir) = &t.cwd {
                self.follow_cwd(t.id, dir);
            }
        }
        self.saved_last.clear();
        self.dirty = true;
        Ok(saved.current.filter(|c| self.views.contains_key(c)))
    }

    /// Tell the background pmux the current layout, if it changed since last time.
    pub fn sync_layout(&mut self) {
        let Some(link) = &self.hold else { return };
        let term = |id: &u32| self.panes.get(id).is_some_and(Pane::is_term);
        let mut views: Vec<_> = self.views.iter().collect();
        views.sort_by(|a, b| a.0.cmp(b.0));
        let views = views
            .into_iter()
            .map(|(path, v)| SavedView {
                path: path.clone(),
                tab: v.tab,
                tabs: v.tabs.iter().map(|t| SavedTab { panes: t.panes.iter().copied().filter(term).collect(), focus: t.focus }).collect(),
            })
            .collect();
        let s = serde_json::to_string(&Saved { current: self.current.clone(), views }).unwrap();
        if s != self.saved_last {
            link.layout(&s);
            self.saved_last = s;
        }
        // Titles and explicit states live in the background pmux, so commands sent while the
        // window is closed see them (and can change them).
        for p in self.panes.values().filter(|p| p.is_term()) {
            let m = Meta { title: p.title.clone(), state: p.state.map(|s| s.as_str().to_string()) };
            if self.meta_last.get(&p.id) != Some(&m) {
                link.meta(p.id, &m);
                self.meta_last.insert(p.id, m);
            }
        }
        self.meta_last.retain(|id, _| self.panes.contains_key(id));
    }

    /// The background pmux went away, and its terminals with it.
    pub(super) fn on_hold_lost(&mut self) {
        self.hold = None;
        let ids: Vec<u32> = self.panes.values().filter(|p| p.is_term()).map(|p| p.id).collect();
        for id in ids {
            self.on_exit(id);
        }
        self.say("background pmux stopped: its terminals ended");
    }
}
