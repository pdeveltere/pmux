//! Desktop integration through the platform's CLI tools (clipboard, file manager). No extra crates.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const MACOS: bool = cfg!(target_os = "macos");

/// Menu label for `reveal`.
pub const REVEAL_LABEL: &str = if MACOS { "Reveal in Finder" } else { "Open folder" };

/// Clipboard text (`pbpaste`, else `wl-paste` / `xclip`).
pub fn clipboard_get() -> Option<String> {
    let cmds: &[&[&str]] = if MACOS { &[&["pbpaste"]] } else { &[&["wl-paste", "-n"], &["xclip", "-o", "-selection", "clipboard"]] };
    cmds.iter().find_map(|c| {
        let o = Command::new(c[0]).args(&c[1..]).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    })
}

/// Put `s` on the clipboard (`pbcopy`, else `wl-copy` / `xclip`).
pub fn clipboard_set(s: &str) {
    let cmds: &[&[&str]] = if MACOS { &[&["pbcopy"]] } else { &[&["wl-copy"], &["xclip", "-selection", "clipboard"]] };
    for c in cmds {
        if let Ok(mut ch) = Command::new(c[0]).args(&c[1..]).stdin(Stdio::piped()).spawn() {
            if let Some(mut i) = ch.stdin.take() {
                let _ = i.write_all(s.as_bytes());
            }
            let _ = ch.wait();
            return;
        }
    }
}

/// Show `p` in the file manager: a file is selected in Finder; elsewhere its folder opens.
pub fn reveal(p: &Path) {
    let mut c = Command::new(if MACOS { "open" } else { "xdg-open" });
    if MACOS {
        if !p.is_dir() {
            c.arg("-R");
        }
        c.arg(p);
    } else if p.is_dir() {
        c.arg(p);
    } else if let Some(d) = p.parent() {
        c.arg(d);
    }
    let _ = c.spawn();
}
