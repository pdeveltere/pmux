//! Keyboard: app shortcuts (⌘ and the C-a prefix), prompts, and routing keys to the focused part.
use super::{App, Focus, Panel, Prompt, PromptKind};
use crate::{config, input, platform};
use alacritty_terminal::term::TermMode;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{ModifiersState, NamedKey};

impl App {
    pub fn on_paste(&mut self, s: &str) {
        if let Some(f) = &mut self.finder {
            f.query.extend(s.chars().filter(|c| !c.is_control()));
            return f.update();
        }
        if let Some(p) = &mut self.prompt {
            p.input.push_str(s.trim());
            return;
        }
        if self.has_focused_editor() {
            return self.editor_paste(s);
        }
        if let Some(id) = self.focused_search() {
            return self.search_paste(id, s);
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
        if self.finder.is_some() {
            return self.finder_key(k, ch);
        }
        // App shortcuts on Cmd (Super).
        if m.super_key() {
            if self.prompt.is_none() && (self.editor_cmd_key(k, ch) || self.search_cmd_key(k)) {
                return;
            }
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
        if self.panes[&id].editor.is_some() {
            return self.editor_key(k);
        }
        if self.panes[&id].search.is_some() {
            return self.search_tab_key(id, k);
        }
        let mode = self.panes[&id].vt.mode();
        let kitty = mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL);
        let bytes = input::encode(k, m, mode.contains(TermMode::APP_CURSOR), kitty);
        if !bytes.is_empty() {
            self.type_pane(id, &bytes);
        }
    }

    fn cmd_key(&mut self, k: &KeyEvent, ch: Option<char>) {
        match (input::named(k), ch.map(|c| c.to_ascii_lowercase())) {
            (_, Some('k')) => self.clear_focused(),
            (_, Some('c')) => {
                if let Some(s) = self.term_selection_text() {
                    platform::clipboard_set(&s);
                }
            }
            (_, Some('p')) => self.open_finder(),
            (_, Some('f')) if self.mods.shift_key() => self.open_project_search(),
            (_, Some('v')) => {
                if let Some(s) = platform::clipboard_get() {
                    self.on_paste(&s);
                }
            }
            (_, Some('d')) => self.split(),
            (_, Some('t')) if self.mods.shift_key() => self.reopen_closed(),
            (_, Some('t')) => self.new_tab(),
            (_, Some('{')) => self.select_tab(-1, true),
            (_, Some('}')) => self.select_tab(1, true),
            (_, Some(c @ '1'..='9')) => self.select_tab(c as isize - '1' as isize, false),
            (_, Some('w')) => self.close_focused(),
            (_, Some('[')) => self.move_focus(-1),
            (_, Some(']')) => self.move_focus(1),
            (_, Some('b')) => self.toggle_panel(Panel::Sidebar),
            (_, Some('a')) if self.mods.shift_key() => self.toggle_panel(Panel::Agents),
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
            (_, Some('b')) => self.toggle_panel(Panel::Sidebar),
            (_, Some('a')) => self.toggle_panel(Panel::Agents),
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
        match input::named(k) {
            Some(NamedKey::Escape) => return self.end_inline(&p),
            Some(NamedKey::Enter) => {
                self.end_inline(&p);
                return self.submit(p);
            }
            Some(NamedKey::Backspace) => {
                p.input.pop();
            }
            Some(NamedKey::Tab) => {
                match &p.kind {
                    // A new project's name is relative to its workspace's folder.
                    PromptKind::AddProject(ws) if !p.input.starts_with(['/', '~']) => {
                        if let Some(w) = self.ws_index(ws) {
                            let root = format!("{}/", self.workspaces[w].path.display().to_string().trim_end_matches('/'));
                            let done = complete_dir(&format!("{root}{}", p.input));
                            p.input = done.strip_prefix(&root).map_or(p.input.clone(), str::to_string);
                        }
                    }
                    PromptKind::AddProject(_) | PromptKind::NewWorkspace => p.input = complete_dir(&p.input),
                    _ => {}
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
            // By name: the workspace may have moved (or gone) while the prompt was open.
            PromptKind::AddProject(ws) => match self.ws_index(&ws) {
                Some(w) => self.new_project(w, input).map(|name| format!("added {name}")),
                None => Err(format!("workspace {ws} no longer exists")),
            },
            PromptKind::NewWorkspace => self.add_workspace(input).map(|name| format!("added workspace {name}")),
            PromptKind::RenameWorkspace(ws) => match self.ws_index(&ws) {
                Some(w) => self.rename_workspace(w, input).map(|()| String::new()),
                None => Err(format!("workspace {ws} no longer exists")),
            },
            PromptKind::NewWorktree(proj) => return self.add_worktree(&proj, input),
        };
        match r {
            Ok(m) if m.is_empty() => {}
            Ok(m) | Err(m) => self.say(m),
        }
    }
}

/// Tab completion of a directory path: extend to the longest common prefix of matching folders.
fn complete_dir(input: &str) -> String {
    // Split the input itself at its last `/`: the name being completed is the text after it.
    // (Splitting the expanded path instead breaks on `~`, `.` and `..`, whose expansions differ
    // in length from what was typed.)
    if input == "~" {
        return "~/".into();
    }
    let (base, pre) = match input.rfind('/') {
        Some(i) => (&input[..=i], &input[i + 1..]),
        None => ("", input),
    };
    if pre == "." || pre == ".." {
        return format!("{input}/");
    }
    let dir = if base.is_empty() { std::path::PathBuf::from(".") } else { config::expand(base) };
    let Ok(rd) = std::fs::read_dir(&dir) else { return input.into() };
    let m: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(pre) && (!n.starts_with('.') || pre.starts_with('.')))
        .collect();
    let Some(first) = m.first() else { return input.into() };
    let mut common = first.clone();
    for n in &m[1..] {
        while !n.starts_with(&common) {
            common.pop();
        }
    }
    format!("{base}{common}{}", if m.len() == 1 { "/" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::complete_dir;

    #[test]
    fn complete_dir_never_panics_on_short_or_special_input() {
        assert_eq!(complete_dir("~"), "~/");
        assert_eq!(complete_dir(".."), "../");
        assert_eq!(complete_dir("~/."), "~/./");
        let _ = complete_dir("");
        let _ = complete_dir("/");
        let _ = complete_dir("x/.");
        let root = env!("CARGO_MANIFEST_DIR");
        assert_eq!(complete_dir(&format!("{root}/sr")), format!("{root}/src/"));
    }
}
