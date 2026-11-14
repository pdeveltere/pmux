//! The socket API: `pmux <command>` from inside or outside a pane.
use super::{App, Close, Pane, State};
use crate::api::{Reply, Request};
use crate::{config, git};
use std::path::PathBuf;

/// `pmux status` values: the states, plus `clear` to go back to the heuristic.
const STATUS_ARGS: &str = "working|waiting|idle|done|error|clear";

fn here(id: u32, me: Option<u32>) -> &'static str {
    if Some(id) == me {
        "  ◀ here"
    } else {
        ""
    }
}

fn state_str(p: &Pane) -> &'static str {
    p.state().map_or("-", State::as_str)
}

impl App {
    /// A pane by `%N`, `N`, `self` (the caller) or title.
    fn resolve(&self, r: &str, caller: Option<&str>) -> Option<u32> {
        let r = if r == "self" { caller? } else { r };
        if let Ok(n) = r.trim_start_matches('%').parse::<u32>() {
            return self.panes.contains_key(&n).then_some(n);
        }
        self.panes.values().find(|p| p.label() == r).map(|p| p.id)
    }

    fn pane_line(&self, p: &Pane, me: Option<u32>) -> String {
        let (ws, proj, wt) = self.names(&p.view);
        format!("%{:<3} {:<8} {:<8} {:<36} {}{}", p.id, p.kind(), state_str(p), format!("{ws}/{proj}/{wt}"), p.label(), here(p.id, me))
    }

    /// Ask-free close for the API: refuses while panes are open unless forced.
    fn api_close(&mut self, c: Close, force: bool) -> Reply {
        let (name, _, n) = self.close_info(&c);
        if n > 0 && !force {
            return Reply::err(format!("{name} has {n} open pane(s); add --force to close them"));
        }
        self.close(c);
        Reply::ok(format!("closed {name}"))
    }

    pub(super) fn on_api(&mut self, req: Request) -> Reply {
        let caller = req.caller.as_deref();
        let me = caller.and_then(|c| self.resolve(c, None));
        let a: Vec<&str> = req.args.iter().map(String::as_str).collect();
        let force = a.contains(&"--force");
        match a.as_slice() {
            ["ls", ..] => {
                let mut ps: Vec<&Pane> = self.panes.values().collect();
                ps.sort_by_key(|p| p.id);
                Reply::ok(ps.iter().map(|p| self.pane_line(p, me) + "\n").collect::<String>())
            }
            ["whoami", ..] => match me.and_then(|id| self.panes.get(&id)) {
                Some(p) => Reply::ok(self.pane_line(p, None)),
                None => Reply::err("not inside a pmux pane"),
            },
            ["tree", ..] => Reply::ok(self.tree(me)),
            ["read", t, rest @ ..] => {
                let Some(id) = self.resolve(t, caller) else { return Reply::err(format!("no pane {t}")) };
                let n: usize = rest.first().and_then(|n| n.parse().ok()).unwrap_or(usize::MAX);
                let text = self.panes[&id].vt.text();
                let lines: Vec<&str> = text.trim_end().lines().collect();
                Reply::ok(lines[lines.len().saturating_sub(n)..].join("\n"))
            }
            ["send", t, text @ ..] if !text.is_empty() => {
                let Some(id) = self.resolve(t, caller) else { return Reply::err(format!("no pane {t}")) };
                self.write_pane(id, text.join(" ").as_bytes());
                self.panes[&id].enter_later();
                Reply::ok("")
            }
            ["new", rest @ ..] => {
                let (tab, cmd) = match rest {
                    ["--tab", cmd @ ..] => (true, cmd),
                    cmd => (false, cmd),
                };
                self.api_new(me, tab, cmd)
            }
            ["status", st, rest @ ..] => {
                let Some(id) = rest.first().and_then(|t| self.resolve(t, caller)).or(me) else { return Reply::err("no target pane") };
                let state = match (*st, State::parse(st)) {
                    ("clear", _) => None,
                    (_, Some(s)) => Some(s),
                    _ => return Reply::err(format!("state must be one of {STATUS_ARGS}")),
                };
                if let Some(p) = self.panes.get_mut(&id) {
                    p.state = state;
                }
                Reply::ok("")
            }
            ["title", text @ ..] if !text.is_empty() => match me.and_then(|id| self.panes.get_mut(&id)) {
                Some(p) => {
                    p.title = Some(text.join(" "));
                    Reply::ok("")
                }
                None => Reply::err("not inside a pmux pane"),
            },
            ["ws", "new", name @ ..] if !name.is_empty() => match self.add_workspace(&name.join(" ")) {
                Ok(()) => Reply::ok(""),
                Err(e) => Reply::err(e),
            },
            ["ws", "rm", name, ..] => match self.workspaces.iter().position(|w| w.name == *name) {
                Some(w) => self.api_close(Close::Workspace(w), force),
                None => Reply::err(format!("no workspace {name}")),
            },
            ["project", "add", dir, rest @ ..] => {
                let w = match rest {
                    ["-w", ws, ..] => self.workspaces.iter().position(|w| w.name == *ws),
                    _ => me.and_then(|id| self.panes.get(&id)).and_then(|p| self.ws_of(&p.view)).or(self.workspaces.len().checked_sub(1)),
                };
                let Some(w) = w else { return Reply::err("no such workspace (create one: pmux ws new <name>)") };
                match self.add_project(w, dir) {
                    Ok(name) => Reply::ok(format!("added {name} to {}", self.workspaces[w].name)),
                    Err(e) => Reply::err(e),
                }
            }
            ["project", "rm", t, ..] => {
                let path = std::fs::canonicalize(config::expand(t)).ok();
                let hit = self.workspaces.iter().enumerate().find_map(|(w, ws)| ws.projects.iter().position(|p| Some(&p.path) == path.as_ref() || p.name == *t).map(|p| (w, p)));
                match hit {
                    Some((w, p)) => self.api_close(Close::Project(w, p), force),
                    None => Reply::err(format!("no project {t}")),
                }
            }
            ["worktree", "rm", t, ..] => self.api_worktree_rm(t, force),
            _ => Reply::err(format!("unknown command: {} (see pmux --help)", a.join(" "))),
        }
    }

    /// `pmux tree`: workspaces, projects, worktrees and their panes.
    fn tree(&self, me: Option<u32>) -> String {
        let mut s = String::new();
        for ws in &self.workspaces {
            s += &format!("{}\n", ws.name);
            for p in &ws.projects {
                s += &format!("  {}  {}\n", p.name, config::tilde(&p.path));
                let mut views: Vec<(String, PathBuf)> = p.listed().map(|(_, t)| (t.branch.clone(), t.path.clone())).collect();
                if !views.iter().any(|(_, v)| *v == p.path) {
                    views.insert(0, (String::new(), p.path.clone()));
                }
                for (branch, path) in views {
                    let ind = if branch.is_empty() {
                        "    "
                    } else {
                        s += &format!("    {}  {}\n", branch, config::tilde(&path));
                        "      "
                    };
                    for id in self.panes_in(&path) {
                        let pn = &self.panes[&id];
                        s += &format!("{ind}%{} {} [{}] {}{}\n", id, pn.kind(), state_str(pn), pn.label(), here(id, me));
                    }
                }
            }
        }
        s
    }

    /// `pmux new [--tab] [cmd…]`: from inside a pane, split that pane's tab without changing which
    /// tab is on screen.
    fn api_new(&mut self, me: Option<u32>, tab: bool, cmd: &[&str]) -> Reply {
        let view = me.and_then(|id| self.panes.get(&id)).map(|p| p.view.clone()).or(self.current.clone());
        let Some(view) = view else { return Reply::err("no current worktree") };
        let shown = self.views.get(&view).map(|v| v.tab);
        if let (Some(id), Some(v), false) = (me, self.views.get_mut(&view), tab) {
            v.reveal(id);
        }
        match self.spawn_pane(&view, &view, tab) {
            Ok(id) => {
                if let (Some(t), Some(v), false) = (shown, self.views.get_mut(&view), tab) {
                    v.tab = t;
                }
                if !cmd.is_empty() {
                    self.write_pane(id, format!("{}\r", cmd.join(" ")).as_bytes());
                }
                Reply::ok(format!("%{id}"))
            }
            Err(e) => Reply::err(e),
        }
    }

    fn api_worktree_rm(&mut self, t: &str, force: bool) -> Reply {
        let path = std::fs::canonicalize(config::expand(t)).ok();
        let hit = self.projects().find_map(|p| p.worktrees.iter().flatten().find(|w| !w.main && Some(&w.path) == path.as_ref()).map(|w| (p.path.clone(), w.path.clone())));
        let Some((proj, wt)) = hit else { return Reply::err(format!("no linked worktree at {t}")) };
        let n = self.panes_in(&wt).len();
        if (n > 0 || git::dirty(&wt)) && !force {
            return Reply::err(format!("worktree has {n} open pane(s) or uncommitted changes; add --force"));
        }
        self.close(Close::Worktree(proj, wt.clone(), force));
        if wt.exists() {
            Reply::err(self.flash.as_ref().map(|f| f.0.clone()).unwrap_or_default())
        } else {
            Reply::ok(format!("deleted {}", config::tilde(&wt)))
        }
    }
}
