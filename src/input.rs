//! winit key events → bytes for the PTY (xterm encoding).
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState as M, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

fn modnum(m: M) -> u8 {
    1 + m.shift_key() as u8 + 2 * m.alt_key() as u8 + 4 * m.control_key() as u8
}

/// The named key (arrows, Enter, …), if it is one.
pub fn named(k: &KeyEvent) -> Option<NamedKey> {
    match &k.logical_key {
        Key::Named(n) => Some(*n),
        _ => None,
    }
}

/// The first character the key produces, with all modifiers applied.
pub fn char(k: &KeyEvent) -> Option<char> {
    match &k.logical_key {
        Key::Character(s) => s.chars().next(),
        _ => None,
    }
}

/// The key's character ignoring Ctrl/Alt (Option on macOS), keeping Shift.
pub fn base_char(k: &KeyEvent, m: M) -> Option<char> {
    let c = match k.key_without_modifiers() {
        Key::Character(s) => s.chars().next()?,
        _ => return None,
    };
    Some(if m.shift_key() { c.to_ascii_uppercase() } else { c })
}

/// Bytes a key press sends to the PTY, as xterm encodes it. `app_cursor`: DECCKM is on
/// (arrows send `ESC O x` instead of `ESC [ x`).
pub fn encode(k: &KeyEvent, m: M, app_cursor: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let csi = |s: &str| format!("\x1b[{s}").into_bytes();
    let arrow = |c: char| -> Vec<u8> {
        if m.shift_key() || m.alt_key() || m.control_key() {
            format!("\x1b[1;{}{c}", modnum(m)).into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if m.shift_key() || m.alt_key() || m.control_key() {
            csi(&format!("{n};{}~", modnum(m)))
        } else {
            csi(&format!("{n}~"))
        }
    };
    match &k.logical_key {
        Key::Named(n) => match n {
            NamedKey::Enter => {
                if m.alt_key() {
                    out.push(0x1b)
                }
                out.push(b'\r')
            }
            NamedKey::Backspace => {
                if m.alt_key() {
                    out.push(0x1b)
                }
                out.push(if m.control_key() { 0x08 } else { 0x7f })
            }
            NamedKey::Tab => out = if m.shift_key() { csi("Z") } else { vec![b'\t'] },
            NamedKey::Escape => out.push(0x1b),
            NamedKey::Space => {
                if m.alt_key() {
                    out.push(0x1b)
                }
                out.push(if m.control_key() { 0 } else { b' ' })
            }
            NamedKey::ArrowUp => out = arrow('A'),
            NamedKey::ArrowDown => out = arrow('B'),
            NamedKey::ArrowRight => out = arrow('C'),
            NamedKey::ArrowLeft => out = arrow('D'),
            NamedKey::Home => out = arrow('H'),
            NamedKey::End => out = arrow('F'),
            NamedKey::PageUp => out = tilde(5),
            NamedKey::PageDown => out = tilde(6),
            NamedKey::Delete => out = tilde(3),
            NamedKey::Insert => out = tilde(2),
            NamedKey::F1 => out = b"\x1bOP".to_vec(),
            NamedKey::F2 => out = b"\x1bOQ".to_vec(),
            NamedKey::F3 => out = b"\x1bOR".to_vec(),
            NamedKey::F4 => out = b"\x1bOS".to_vec(),
            NamedKey::F5 => out = tilde(15),
            NamedKey::F6 => out = tilde(17),
            NamedKey::F7 => out = tilde(18),
            NamedKey::F8 => out = tilde(19),
            NamedKey::F9 => out = tilde(20),
            NamedKey::F10 => out = tilde(21),
            NamedKey::F11 => out = tilde(23),
            NamedKey::F12 => out = tilde(24),
            _ => {}
        },
        Key::Character(_) => {
            if m.control_key() {
                if let Some(c) = base_char(k, M::empty()) {
                    if m.alt_key() {
                        out.push(0x1b);
                    }
                    out.push(match c.to_ascii_lowercase() {
                        c @ 'a'..='z' => c as u8 - b'a' + 1,
                        ' ' | '@' | '2' => 0,
                        '[' | '3' => 27,
                        '\\' | '4' => 28,
                        ']' | '5' => 29,
                        '^' | '6' => 30,
                        '_' | '-' | '7' => 31,
                        '?' | '8' => 127,
                        c => c as u8,
                    });
                    return out;
                }
            }
            // Option acts as Alt (Meta): ESC + the unmodified key.
            if m.alt_key() {
                if let Some(c) = base_char(k, m) {
                    out.push(0x1b);
                    let mut b = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                    return out;
                }
            }
            if let Some(t) = &k.text {
                out.extend_from_slice(t.as_bytes());
            }
        }
        _ => {
            if let Some(t) = &k.text {
                out.extend_from_slice(t.as_bytes());
            }
        }
    }
    out
}
