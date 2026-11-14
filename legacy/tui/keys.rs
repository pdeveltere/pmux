use crossterm::event::{KeyCode, KeyEvent, KeyModifiers as M};

fn modnum(m: M) -> u8 {
    1 + m.contains(M::SHIFT) as u8 + 2 * m.contains(M::ALT) as u8 + 4 * m.contains(M::CONTROL) as u8
}

pub fn encode(k: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let m = k.modifiers;
    let mut out = Vec::new();
    let csi = |s: &str| format!("\x1b[{s}").into_bytes();
    let arrow = |c: char| -> Vec<u8> {
        if m.intersects(M::SHIFT | M::ALT | M::CONTROL) {
            format!("\x1b[1;{}{c}", modnum(m)).into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    match k.code {
        KeyCode::Char(c) => {
            if m.contains(M::ALT) {
                out.push(0x1b);
            }
            if m.contains(M::CONTROL) {
                let b = match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    'A'..='Z' => c as u8 - b'A' + 1,
                    ' ' | '@' | '2' => 0,
                    '[' | '3' => 27,
                    '\\' | '4' => 28,
                    ']' | '5' => 29,
                    '^' | '6' => 30,
                    '_' | '-' | '7' => 31,
                    '?' | '8' => 127,
                    _ => c as u8,
                };
                out.push(b);
            } else {
                let mut b = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            return out;
        }
        KeyCode::Enter => {
            if m.contains(M::ALT) {
                out.push(0x1b)
            }
            out.push(b'\r')
        }
        KeyCode::Backspace => {
            if m.contains(M::ALT) {
                out.push(0x1b)
            }
            out.push(0x7f)
        }
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out = csi("Z"),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up => out = arrow('A'),
        KeyCode::Down => out = arrow('B'),
        KeyCode::Right => out = arrow('C'),
        KeyCode::Left => out = arrow('D'),
        KeyCode::Home => out = arrow('H'),
        KeyCode::End => out = arrow('F'),
        KeyCode::PageUp => out = csi("5~"),
        KeyCode::PageDown => out = csi("6~"),
        KeyCode::Delete => out = csi("3~"),
        KeyCode::Insert => out = csi("2~"),
        KeyCode::F(n) => {
            out = match n {
                1..=4 => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
                5 => csi("15~"),
                6..=10 => csi(&format!("{}~", n + 11)),
                11 | 12 => csi(&format!("{}~", n + 12)),
                _ => vec![],
            }
        }
        _ => {}
    }
    out
}
