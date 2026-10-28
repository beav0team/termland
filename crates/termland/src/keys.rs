//! Kitterm input handling and the old-school ANSI encoder that turns a
//! `crossterm` key event back into bytes for the child pty.
//!
//! This is the part every multiplexer gets subtly wrong, so it is isolated and
//! unit tested: nothing here touches the terminal, it only maps events to
//! byte sequences.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Encode a key event as the bytes an xterm-256color application expects.
/// Returns `None` for keys with no classic representation (they are simply
/// dropped rather than sent as garbage).
pub fn encode(ev: KeyEvent) -> Option<Vec<u8>> {
    let m = ev.modifiers;
    let shift = m.contains(KeyModifiers::SHIFT);
    let alt = m.contains(KeyModifiers::ALT);
    let ctrl = m.contains(KeyModifiers::CONTROL);

    // xterm modifier parameter: 1 + 1*shift + 2*alt + 4*ctrl
    let mparam = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;

    let mut out: Vec<u8> = Vec::new();

    match ev.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl+<char> folds into the C0 range.
                let byte = match c {
                    'a'..='z' => Some(c as u8 - b'a' + 1),
                    'A'..='Z' => Some(c as u8 - b'A' + 1),
                    ' ' | '@' => Some(0),
                    '[' => Some(27),
                    '\\' => Some(28),
                    ']' => Some(29),
                    '^' => Some(30),
                    '_' => Some(31),
                    '?' => Some(127),
                    _ => None,
                };
                match byte {
                    Some(b) => {
                        if alt {
                            out.push(0x1b);
                        }
                        out.push(b);
                    }
                    None => {
                        // No C0 for this one; fall back to ESC + literal.
                        if alt {
                            out.push(0x1b);
                        }
                        out.extend(c.to_string().as_bytes());
                    }
                }
            } else if alt {
                out.push(0x1b);
                out.extend(c.to_string().as_bytes());
            } else {
                out.extend(c.to_string().as_bytes());
            }
        }
        KeyCode::Enter => {
            if alt {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        KeyCode::Tab => {
            if shift {
                out.extend(b"\x1b[Z");
            } else {
                out.push(b'\t');
            }
        }
        KeyCode::BackTab => out.extend(b"\x1b[Z"),
        KeyCode::Backspace => {
            if alt {
                out.push(0x1b);
            }
            out.push(0x7f);
        }
        KeyCode::Esc => {
            out.push(0x1b);
            if alt {
                out.push(0x1b);
            }
        }
        KeyCode::Null => out.push(0),
        KeyCode::Delete => csi_tilde(&mut out, 3, mparam),
        KeyCode::Insert => csi_tilde(&mut out, 2, mparam),
        KeyCode::PageUp => csi_tilde(&mut out, 5, mparam),
        KeyCode::PageDown => csi_tilde(&mut out, 6, mparam),
        KeyCode::Home => csi_letter(&mut out, b'H', mparam),
        KeyCode::End => csi_letter(&mut out, b'F', mparam),
        KeyCode::Up => csi_letter(&mut out, b'A', mparam),
        KeyCode::Down => csi_letter(&mut out, b'B', mparam),
        KeyCode::Right => csi_letter(&mut out, b'C', mparam),
        KeyCode::Left => csi_letter(&mut out, b'D', mparam),
        KeyCode::F(n) => {
            let seq: &[u8] = match n {
                1 => b"P",
                2 => b"Q",
                3 => b"R",
                4 => b"S",
                5 => b"15~",
                6 => b"17~",
                7 => b"18~",
                8 => b"19~",
                9 => b"20~",
                10 => b"21~",
                11 => b"23~",
                12 => b"24~",
                _ => return None,
            };
            if mparam == 1 {
                out.extend(b"\x1b[");
                out.extend(seq);
            } else {
                // Modified F1..F4 are ESC [ 1 ; m P..S
                // Modified F5..F12 are ESC [ 15 ; m ~  (etc.)
                let body = String::from_utf8_lossy(seq);
                match body.strip_suffix('~') {
                    Some(code) => out.extend(format!("\x1b[{code};{mparam}~").into_bytes()),
                    None => out.extend(format!("\x1b[1;{mparam}{body}").into_bytes()),
                }
            }
        }
        _ => return None,
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn csi_letter(out: &mut Vec<u8>, final_byte: u8, mparam: u8) {
    if mparam == 1 {
        out.extend([0x1b, b'[']);
        out.push(final_byte);
    } else {
        out.extend(format!("\x1b[1;{mparam}").into_bytes());
        out.push(final_byte);
    }
}

fn csi_tilde(out: &mut Vec<u8>, code: u8, mparam: u8) {
    if mparam == 1 {
        out.extend(format!("\x1b[{code}~").into_bytes());
    } else {
        out.extend(format!("\x1b[{code};{mparam}~").into_bytes());
    }
}

/// Is this event the leader key (`Ctrl+Space`)?
///
/// Terminals without the kitty keyboard protocol report `Ctrl+Space` as NUL,
/// so both spellings are accepted; `crossterm` surfaces NUL as `Char('\0')`
/// and as `KeyCode::Null` depending on version and terminal.
pub fn is_leader(ev: &KeyEvent) -> bool {
    if ev.kind != KeyEventKind::Press && ev.kind != KeyEventKind::Repeat {
        return false;
    }
    match ev.code {
        KeyCode::Char(' ') => ev.modifiers.contains(KeyModifiers::CONTROL),
        KeyCode::Char('\0') | KeyCode::Null => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn plain_text_passes_through() {
        let got = encode(k(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        assert_eq!(got, b"a");
        let got = encode(k(KeyCode::Char('Z'), KeyModifiers::SHIFT)).unwrap();
        assert_eq!(got, b"Z");
    }

    #[test]
    fn ctrl_letters_become_c0() {
        assert_eq!(
            encode(k(KeyCode::Char('c'), KeyModifiers::CONTROL)).unwrap(),
            vec![3]
        );
        assert_eq!(
            encode(k(KeyCode::Char('d'), KeyModifiers::CONTROL)).unwrap(),
            vec![4]
        );
        assert_eq!(
            encode(k(KeyCode::Char('['), KeyModifiers::CONTROL)).unwrap(),
            vec![27]
        );
        assert_eq!(
            encode(k(KeyCode::Char(' '), KeyModifiers::CONTROL)).unwrap(),
            vec![0]
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            encode(k(KeyCode::Char('b'), KeyModifiers::ALT)).unwrap(),
            b"\x1bb"
        );
    }

    #[test]
    fn arrows_and_modifiers() {
        assert_eq!(
            encode(k(KeyCode::Up, KeyModifiers::NONE)).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            encode(k(KeyCode::Up, KeyModifiers::SHIFT)).unwrap(),
            b"\x1b[1;2A"
        );
        assert_eq!(
            encode(k(KeyCode::Right, KeyModifiers::CONTROL)).unwrap(),
            b"\x1b[1;5C"
        );
        assert_eq!(
            encode(k(KeyCode::Delete, KeyModifiers::NONE)).unwrap(),
            b"\x1b[3~"
        );
        assert_eq!(
            encode(k(KeyCode::F(1), KeyModifiers::NONE)).unwrap(),
            b"\x1b[P"
        );
        assert_eq!(
            encode(k(KeyCode::F(5), KeyModifiers::NONE)).unwrap(),
            b"\x1b[15~"
        );
        assert_eq!(
            encode(k(KeyCode::F(5), KeyModifiers::SHIFT)).unwrap(),
            b"\x1b[15;2~"
        );
    }

    #[test]
    fn editing_keys() {
        assert_eq!(
            encode(k(KeyCode::Enter, KeyModifiers::NONE)).unwrap(),
            b"\r"
        );
        assert_eq!(encode(k(KeyCode::Tab, KeyModifiers::NONE)).unwrap(), b"\t");
        assert_eq!(
            encode(k(KeyCode::BackTab, KeyModifiers::SHIFT)).unwrap(),
            b"\x1b[Z"
        );
        assert_eq!(
            encode(k(KeyCode::Backspace, KeyModifiers::NONE)).unwrap(),
            vec![0x7f]
        );
        assert_eq!(
            encode(k(KeyCode::Esc, KeyModifiers::NONE)).unwrap(),
            vec![0x1b]
        );
    }

    #[test]
    fn leader_is_ctrl_space_in_both_encodings() {
        assert!(is_leader(&k(KeyCode::Char(' '), KeyModifiers::CONTROL)));
        assert!(is_leader(&k(KeyCode::Null, KeyModifiers::NONE)));
        assert!(!is_leader(&k(KeyCode::Char(' '), KeyModifiers::NONE)));
    }

    #[test]
    fn every_modifier_combo_roundtrips_to_a_nonempty_sequence() {
        let codes = [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ];
        for c in codes {
            for m in [
                KeyModifiers::SHIFT,
                KeyModifiers::ALT,
                KeyModifiers::CONTROL,
                KeyModifiers::SHIFT | KeyModifiers::CONTROL,
            ] {
                let seq = encode(k(c, m)).unwrap_or_else(|| panic!("{c:?} {m:?} produced nothing"));
                assert!(seq.starts_with(b"\x1b"), "{c:?} {m:?} -> {seq:?}");
                assert!(seq.len() > 2);
            }
        }
    }
}
