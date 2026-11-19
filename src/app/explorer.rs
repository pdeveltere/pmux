//! File explorer panel on the right: showing/hiding, keys, clicks, opening files.
use super::{item, sep, Action, App, Ev, Focus, MenuItem};
use crate::files::Explorer;
use crate::{git, input, platform, util};
use std::path::Path;
use std::time::{Duration, Instant};
use winit::event::{KeyEvent, MouseButton};
use winit::keyboard::NamedKey;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

impl App {
    pub fn explorer(&self) -> Option<&Explorer> {
        self.files.get(self.current.as_ref()?)
    }

    pub(super) fn explorer_mut(&mut self) -> Option<&mut Explorer> {
        self.files.get_mut(self.current.as_ref()?)
    }

    /// ⌘⇧E: show and focus the explorer; when it already has focus, hide it.
    pub(super) fn toggle_files(&mut self) {
        if self.explorer().is_none() {
            return self.say("open a worktree to browse its files");
        }
        if self.show_files && self.focus == Focus::Files {
            return self.hide_files();
        }
        self.show_files_panel();
        self.focus = Focus::Files;
        let vis = self.files_visible_rows();
        if let Some(e) = self.explorer_mut() {
            e.reveal_sel(vis);
        }
    }

    /// Show the explorer, leaving the focus where it is.
    pub(super) fn show_files_panel(&mut self) {
        if self.explorer().is_none() {
            return self.say("open a worktree to browse its files");
        }
        if self.show_files {
            return;
        }
        self.show_files = true;
        if let Some(e) = self.explorer_mut() {
            e.rebuild();
        }
        self.files_rewatch();
        self.refresh_status();
    }

    pub(super) fn hide_files(&mut self) {
        self.show_files = false;
        self.files_hover = None;
        self.link_hover = None;
        if self.focus == Focus::Files {
            self.focus_default();
        }
        self.files_rewatch();
    }

    /// Watch the shown explorer's expanded folders (nothing while it's hidden).
    pub(super) fn files_rewatch(&self) {
        let dirs = self.explorer().filter(|_| self.show_files).map(Explorer::watch_dirs).unwrap_or_default();
        self.files_watcher.set(dirs);
    }

    /// Off the UI thread: `git status` for the shown explorer. At most one runs at a time.
    pub(super) fn refresh_status(&mut self) {
        let Some(root) = self.explorer().filter(|_| self.show_files).map(|e| e.root.clone()) else { return };
        if self.status_busy {
            self.status_again = true;
            return;
        }
        self.status_busy = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let st = git::status(&root);
            let _ = tx.send_event(Ev::FileStatus(root, st));
        });
    }

    /// Expand or collapse explorer row `i`.
    pub(super) fn files_toggle(&mut self, i: usize) {
        let vis = self.files_visible_rows();
        if let Some(e) = self.explorer_mut() {
            e.toggle(i);
            e.clamp_top(vis);
        }
        self.files_rewatch();
    }

    pub(super) fn files_scroll(&mut self, rows: i32) {
        let vis = self.files_visible_rows();
        if let Some(e) = self.explorer_mut() {
            e.top = e.top.saturating_add_signed(-rows as isize);
            e.clamp_top(vis);
        }
    }

    pub(super) fn files_click(&mut self, b: MouseButton, x: f32, y: f32) {
        let hit = self.files_row_at(y);
        self.focus = Focus::Files;
        if b == MouseButton::Right {
            return self.open_files_menu(hit, x, y);
        }
        let (Some(i), MouseButton::Left) = (hit, b) else { return };
        let now = Instant::now();
        let Some(e) = self.explorer_mut() else { return };
        let double = e.last_click.is_some_and(|(j, t)| j == i && now - t < DOUBLE_CLICK);
        e.last_click = Some((i, now));
        e.sel = i;
        if e.entries[i].dir {
            self.files_toggle(i);
        } else if double {
            let p = e.entries[i].path.clone();
            self.open_file(&p);
        }
    }

    pub(super) fn files_key(&mut self, k: &KeyEvent, ch: Option<char>) {
        let named = input::named(k);
        let is = |n: NamedKey, c: char| named == Some(n) || ch == Some(c);
        let vis = self.files_visible_rows();
        let Some(e) = self.explorer_mut() else { return };
        let n = e.entries.len();
        let i = e.sel.min(n - 1);
        let dir = e.entries[i].dir;
        let (mut toggled, mut open, mut leave) = (false, None, false);
        let mut toggle = |e: &mut Explorer| {
            e.toggle(i);
            toggled = true;
        };
        if is(NamedKey::ArrowDown, 'j') {
            e.sel = (i + 1).min(n - 1);
        } else if is(NamedKey::ArrowUp, 'k') {
            e.sel = i.saturating_sub(1);
        } else if is(NamedKey::ArrowRight, 'l') && dir {
            // Right: expand, or step into an expanded folder. Left: collapse, or go to the parent.
            if e.is_open(i) {
                e.sel = (i + 1).min(n - 1);
            } else {
                toggle(e);
            }
        } else if is(NamedKey::ArrowLeft, 'h') {
            if e.is_open(i) {
                toggle(e);
            } else if let Some(p) = e.parent(i) {
                e.sel = p;
            }
        } else {
            match named {
                Some(NamedKey::PageDown) => e.sel = (i + vis).min(n - 1),
                Some(NamedKey::PageUp) => e.sel = i.saturating_sub(vis),
                Some(NamedKey::Home) => e.sel = 0,
                Some(NamedKey::End) => e.sel = n - 1,
                Some(NamedKey::Enter | NamedKey::Space) if dir => toggle(e),
                Some(NamedKey::Enter) => open = Some(e.entries[i].path.clone()),
                Some(NamedKey::Escape | NamedKey::Tab) => leave = true,
                _ => {}
            }
        }
        e.reveal_sel(vis);
        if toggled {
            self.files_rewatch();
        }
        if let Some(p) = open {
            self.open_file(&p);
        }
        if leave {
            self.focus_default();
        }
    }

    fn open_files_menu(&mut self, row: Option<usize>, x: f32, y: f32) {
        let Some(i) = row else { return };
        let Some(e) = self.explorer_mut() else { return };
        e.sel = i;
        let en = &e.entries[i];
        let path = en.path.clone();
        let rel = path.strip_prefix(&e.root).map(|r| r.display().to_string()).unwrap_or_default();
        let mut items: Vec<MenuItem> = if en.dir {
            let label = if e.is_open(i) { "Collapse" } else { "Expand" };
            vec![item(label, Action::FileToggle(path.clone())), item("New terminal here", Action::Terminal(path.clone()))]
        } else {
            let parent = path.parent().map_or_else(|| e.root.clone(), Path::to_path_buf);
            vec![
                item("Open", Action::OpenFile(path.clone())),
                item("Open in $EDITOR", Action::EditFile(path.clone())),
                item("New terminal here", Action::Terminal(parent)),
            ]
        };
        items.push(sep());
        items.push(item("Copy path", Action::CopyPath(path.clone())));
        if !rel.is_empty() {
            items.push(item("Copy relative path", Action::Copy(rel)));
        }
        items.push(item(platform::REVEAL_LABEL, Action::Reveal(path)));
        self.show_menu(items, x, y);
    }

    /// Open a file in `$EDITOR`, in a new terminal tab of the current view (the shell stays when the editor quits).
    pub(super) fn edit_in_terminal(&mut self, p: &Path) {
        let Some(view) = self.current.clone() else { return };
        let arg = p.strip_prefix(&view).ok().filter(|r| !r.as_os_str().is_empty()).unwrap_or(p);
        let cmd = format!("${{EDITOR:-vi}} {}\r", util::shell_quote(&arg.display().to_string()));
        if let Some(id) = self.spawn_here(None, true) {
            self.write_pane(id, cmd.as_bytes());
        }
    }
}
