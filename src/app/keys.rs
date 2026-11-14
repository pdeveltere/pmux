//! Keyboard: app shortcuts (⌘ and the C-a prefix), prompts, and routing keys to the focused part.
use super::{App, Focus, Prompt, PromptKind};
use crate::{config, input, platform};
use alacritty_terminal::term::TermMode;
use std::path::Path;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

impl App {
    pub fn on_paste(&mut self, s: &str) {
        if let Some(p) = &mut self.prompt {
            p.input.push_str(s.trim());
            return;
        }
        let Some(id) = self.focused().filter(|_| self.focus == Focus::Panes) else { return };
        let bp = self.panes[&id].vt.mode().contains(TermMode::BRACKETED_PASTE);
        let s = s.replace("\r\n", "\r").replace('\n', "\r");
        let data = if bp { format!("\x1b[200~{s}\x1b[201~") } else { s };
        self.type_pane(id, data.as_bytes());
    }

    pub fn on_key(&mut self, k: &KeyEvent) {
        if k.state != ElementState::Pressed {
            return;
        }
        self.dirty = true;
        let m = self.mods;
        let ch = input::char(k);
        if self.menu.is_some() {
            return self.menu_key(k);
        }
        // App shortcuts on Cmd (Super).
        if m.super_key() {
            return self.cmd_key(k, ch);
        }
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        let ctrl_a = m.control_key() && input::base_char(k, ModifiersState::empty()) == Some('a');
        if self.prefix {
            self.prefix = false;
            return self.prefix_key(k, ch, ctrl_a);
        }
        if ctrl_a {
            self.prefix = true;
            return;
        }
        if m.control_key() && input::named(k) == Some(NamedKey::Tab) {
            return self.select_tab(if m.shift_key() { -1 } else { 1 }, true);
        }
        if self.focus == Focus::Files && self.explorer().is_some() {
            return self.files_key(k, ch);
        }
        let id = match self.focused() {
            Some(id) if self.focus != Focus::Sidebar => id,
            _ => return self.sidebar_key(k, ch),
        };
        let ac = self.panes[&id].vt.mode().contains(TermMode::APP_CURSOR);
        let bytes = input::encode(k, m, ac);
        if !bytes.is_empty() {
            self.type_pane(id, &bytes);
        }
    }

    fn cmd_key(&mut self, k: &KeyEvent, ch: Option<char>) {
        match (input::named(k), ch.map(|c| c.to_ascii_lowercase())) {
            (_, Some('k')) => self.clear_focused(),
            (_, Some('v')) => {
                if let Some(s) = platform::clipboard_get() {
                    self.on_paste(&s);
                }
            }
            (_, Some('d')) => self.split(),
            (_, Some('t')) => self.new_tab(),
            (_, Some('{')) => self.select_tab(-1, true),
            (_, Some('}')) => self.select_tab(1, true),
            (_, Some(c @ '1'..='9')) => self.select_tab(c as isize - '1' as isize, false),
            (_, Some('w')) => self.close_focused(),
            (_, Some('[')) => self.move_focus(-1),
            (_, Some(']')) => self.move_focus(1),
            (_, Some('b')) => self.show_sidebar = !self.show_sidebar,
            (_, Some('e')) if self.mods.shift_key() => self.toggle_files(),
            (_, Some('e')) => self.toggle_sidebar_focus(),
            (_, Some('n')) => self.next_attention(),
            (_, Some('q')) => self.quit = true,
            (_, Some('=' | '+')) => self.zoom = Some(1),
            (_, Some('-')) => self.zoom = Some(-1),
            (_, Some('0')) => self.zoom = Some(0),
            (Some(NamedKey::ArrowDown), _) => self.step_view(1),
            (Some(NamedKey::ArrowUp), _) => self.step_view(-1),
            _ => {}
        }
    }

    /// The key after C-a (tmux style). C-a C-a sends a literal C-a.
    fn prefix_key(&mut self, k: &KeyEvent, ch: Option<char>, ctrl_a: bool) {
        if ctrl_a {
            if let Some(id) = self.focused() {
                self.type_pane(id, &[1]);
            }
            return;
        }
        match (input::named(k), ch) {
            (_, Some('|' | '%' | '\\')) => self.split(),
            (_, Some('t')) => self.new_tab(),
            (_, Some(c @ '1'..='9')) => self.select_tab(c as isize - '1' as isize, false),
            (Some(NamedKey::ArrowLeft), _) | (_, Some('h')) => self.move_focus(-1),
            (Some(NamedKey::ArrowRight), _) | (_, Some('l')) => self.move_focus(1),
            (_, Some('x')) => self.close_focused(),
            (Some(NamedKey::Tab), _) | (_, Some('s')) => self.toggle_sidebar_focus(),
            (_, Some('f')) => self.toggle_files(),
            (_, Some('b')) => self.show_sidebar = !self.show_sidebar,
            (Some(NamedKey::ArrowDown), _) | (_, Some('j')) => self.step_view(1),
            (Some(NamedKey::ArrowUp), _) | (_, Some('k')) => self.step_view(-1),
            (_, Some('n')) => self.next_attention(),
            (_, Some('c')) => self.clear_focused(),
            (_, Some('q')) => self.quit = true,
            _ => {}
        }
    }

    fn prompt_key(&mut self, k: &KeyEvent) {
        let Some(mut p) = self.prompt.take() else { return };
        if let PromptKind::Confirm(c) = p.kind {
            let yes = input::named(k) == Some(NamedKey::Enter) || matches!(&k.logical_key, Key::Character(s) if s.as_str() == "y");
            if yes {
                self.close(c);
            }
            return;
        }
        match input::named(k) {
            Some(NamedKey::Escape) => return,
            Some(NamedKey::Enter) => return self.submit(p),
            Some(NamedKey::Backspace) => {
                p.input.pop();
            }
            Some(NamedKey::Tab) => {
                if matches!(p.kind, PromptKind::AddProject(_)) {
                    p.input = complete_dir(&p.input);
                }
            }
            _ => {
                if let Some(t) = &k.text {
                    p.input.extend(t.chars().filter(|c| !c.is_control()));
                }
            }
        }
        self.prompt = Some(p);
    }

    fn submit(&mut self, p: Prompt) {
        let input = p.input.trim();
        if input.is_empty() {
            return;
        }
        let r = match p.kind {
            PromptKind::AddProject(wi) => self.add_project(wi, input).map(|name| format!("added {name}")),
            PromptKind::NewWorkspace => self.add_workspace(input).map(|()| String::new()),
            PromptKind::RenameWorkspace(w) => self.rename_workspace(w, input).map(|()| String::new()),
            PromptKind::NewWorktree(proj) => return self.add_worktree(&proj, input),
            PromptKind::Confirm(c) => return self.close(c),
        };
        match r {
            Ok(m) if m.is_empty() => {}
            Ok(m) | Err(m) => self.say(m),
        }
    }
}

/// Tab completion of a directory path: extend to the longest common prefix of matching folders.
fn complete_dir(input: &str) -> String {
    let full = config::expand(input);
    let (dir, pre) = if input.ends_with('/') {
        (full.clone(), String::new())
    } else {
        (full.parent().map(Path::to_path_buf).unwrap_or_default(), full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
    };
    let Ok(rd) = std::fs::read_dir(&dir) else { return input.into() };
    let m: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&pre) && (!n.starts_with('.') || pre.starts_with('.')))
        .collect();
    let Some(first) = m.first() else { return input.into() };
    let mut common = first.clone();
    for n in &m[1..] {
        while !n.starts_with(&common) {
            common.pop();
        }
    }
    let base = &input[..input.len() - pre.len()];
    format!("{base}{common}{}", if m.len() == 1 { "/" } else { "" })
}
