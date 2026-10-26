//! Mouse support at the two ends of the pty.
//!
//! Outbound: a child program asks for mouse reporting with a DEC private mode
//! (`CSI ? 1000 h` and friends). Nothing in `vt100` exposes that, so the mode
//! is sniffed from the byte stream here, and events are re-encoded into the
//! same wire format the child asked for.
//!
//! Both halves are pure functions so they can be tested without a terminal.

use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// What a child has asked for. Tracking modes are collapsed to one bit: the
/// distinction between 1000/1002/1003 only matters to the child, and motion
/// reports are sent with the motion bit set either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseModes {
    pub tracking: bool,
    /// SGR encoding (1006) instead of the legacy X10 byte offsets.
    pub sgr: bool,
    /// `?1002`: motion is reported while a button is held (drag).
    pub button_motion: bool,
    /// `?1003`: motion is reported for every pointer move, button or not.
    ///
    /// This one is the reason the desktop has to be careful: a pane that asks
    /// for it wants to know where the pointer is *without* a click, and the
    /// app wants the same events for hover. Forwarding is right (the program
    /// asked first); termland's own hover is computed from the same stream.
    pub any_motion: bool,
}

/// Watches the child's output for DEC private mouse modes.
///
/// A short tail is kept between reads so a sequence split across two pty reads
/// is still recognised; that is the same reason `TitleSniffer` keeps one.
#[derive(Default)]
pub struct MouseSniffer {
    tail: Vec<u8>,
}

impl MouseSniffer {
    /// Feed a chunk of child output; `modes` is updated in place.
    pub fn feed(&mut self, bytes: &[u8], modes: &mut MouseModes) {
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > 8192 {
            let drop = self.tail.len() - 1024;
            self.tail.drain(..drop);
        }
        let mut i = 0;
        while i + 2 < self.tail.len() {
            if self.tail[i] == 0x1b && self.tail[i + 1] == b'[' && self.tail[i + 2] == b'?' {
                let mut j = i + 3;
                let mut params = Vec::new();
                let mut cur: Option<u32> = None;
                let mut terminator = None;
                while j < self.tail.len() {
                    match self.tail[j] {
                        b'0'..=b'9' => {
                            let d = (self.tail[j] - b'0') as u32;
                            cur = Some(cur.unwrap_or(0).saturating_mul(10).saturating_add(d));
                        }
                        b';' => {
                            if let Some(v) = cur.take() {
                                params.push(v);
                            }
                        }
                        b'h' | b'l' => {
                            if let Some(v) = cur.take() {
                                params.push(v);
                            }
                            terminator = Some(self.tail[j]);
                            j += 1;
                            break;
                        }
                        _ => break,
                    }
                    j += 1;
                }
                if let Some(t) = terminator {
                    let on = t == b'h';
                    for p in params {
                        match p {
                            1000 => modes.tracking = on,
                            1002 => {
                                modes.tracking = on;
                                modes.button_motion = on;
                            }
                            1003 => {
                                modes.tracking = on;
                                modes.any_motion = on;
                            }
                            1006 => modes.sgr = on,
                            // Turning off every tracking mode also ends SGR.
                            _ => {}
                        }
                    }
                    i = j;
                    continue;
                }
            }
            i += 1;
        }
        // Keep the last few bytes only: a mode sequence is short.
        if self.tail.len() > 64 {
            let drop = self.tail.len() - 64;
            self.tail.drain(..drop);
        }
    }
}

fn modifiers_bits(m: KeyModifiers) -> u8 {
    let mut bits = 0;
    if m.contains(KeyModifiers::SHIFT) {
        bits |= 4;
    }
    if m.contains(KeyModifiers::ALT) {
        bits |= 8;
    }
    if m.contains(KeyModifiers::CONTROL) {
        bits |= 16;
    }
    bits
}

fn button_code(b: MouseButton) -> u8 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Encode one host mouse event for a child that asked for `modes`.
///
/// `origin` is the pane content's top-left cell, so coordinates become 1-based
/// relative to the program's own screen. Returns `None` for events the child
/// did not ask to receive or that fall outside the content area.
pub fn encode(ev: &MouseEvent, modes: MouseModes, origin: (u16, u16), size: (u16, u16)) -> Option<Vec<u8>> {
    if !modes.tracking {
        return None;
    }
    if ev.column < origin.0 || ev.row < origin.1 {
        return None;
    }
    let x = ev.column - origin.0 + 1;
    let y = ev.row - origin.1 + 1;
    if x > size.0 || y > size.1 {
        return None;
    }

    let mut cb = modifiers_bits(ev.modifiers);
    let release;
    match ev.kind {
        MouseEventKind::Down(b) => {
            cb += button_code(b);
            release = false;
        }
        MouseEventKind::Up(b) => {
            cb += button_code(b);
            release = true;
        }
        MouseEventKind::Drag(b) => {
            // Motion bit + held button.
            cb += 32 + button_code(b);
            release = false;
        }
        MouseEventKind::ScrollUp => {
            cb += 64;
            release = false;
        }
        MouseEventKind::ScrollDown => {
            cb += 65;
            release = false;
        }
        MouseEventKind::ScrollLeft => {
            cb += 66;
            release = false;
        }
        MouseEventKind::ScrollRight => {
            cb += 67;
            release = false;
        }
        MouseEventKind::Moved => {
            cb += 32 + 3;
            release = false;
        }
    }

    let mut out = Vec::new();
    if modes.sgr {
        out.extend_from_slice(b"\x1b[<");
        out.extend_from_slice(cb.to_string().as_bytes());
        out.push(b';');
        out.extend_from_slice(x.to_string().as_bytes());
        out.push(b';');
        out.extend_from_slice(y.to_string().as_bytes());
        out.push(if release { b'm' } else { b'M' });
    } else {
        // Legacy X10: release is reported as button 3, no bit for which.
        let b = if release { 3 } else { cb };
        let bx = 32u16.saturating_add(b as u16).min(255) as u8;
        let xx = 32u16.saturating_add(x).min(255) as u8;
        let yy = 32u16.saturating_add(y).min(255) as u8;
        out.extend_from_slice(b"\x1b[M");
        out.push(bx);
        out.push(xx);
        out.push(yy);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row: row,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn sniffing_a_tracking_mode_enables_it() {
        let mut s = MouseSniffer::default();
        let mut m = MouseModes::default();
        s.feed(b"hello\x1b[?1000h world", &mut m);
        assert!(m.tracking);
        assert!(!m.sgr);
        s.feed(b"\x1b[?1006h", &mut m);
        assert!(m.sgr);
        s.feed(b"\x1b[?1000l", &mut m);
        assert!(!m.tracking);
    }

    #[test]
    fn a_mode_sequence_split_across_reads_is_still_seen() {
        let mut s = MouseSniffer::default();
        let mut m = MouseModes::default();
        s.feed(b"\x1b[?1002", &mut m);
        s.feed(b";1006h", &mut m);
        assert!(m.tracking && m.sgr);
    }

    #[test]
    fn sgr_encoding_is_one_based_relative_to_the_pane() {
        let m = MouseModes { tracking: true, sgr: true, button_motion: false, any_motion: false };
        let bytes = encode(&ev(MouseEventKind::Down(MouseButton::Left), 12, 7), m, (10, 5), (80, 24))
            .expect("encoded");
        assert_eq!(bytes, b"\x1b[<0;3;3M".to_vec());

        let release = encode(&ev(MouseEventKind::Up(MouseButton::Left), 12, 7), m, (10, 5), (80, 24))
            .expect("encoded");
        assert_eq!(release, b"\x1b[<0;3;3m".to_vec());
    }

    #[test]
    fn wheel_and_right_button_use_the_expected_codes() {
        let m = MouseModes { tracking: true, sgr: true, button_motion: false, any_motion: false };
        let up = encode(&ev(MouseEventKind::ScrollUp, 11, 6), m, (10, 5), (80, 24)).unwrap();
        assert_eq!(up, b"\x1b[<64;2;2M".to_vec());
        let down = encode(&ev(MouseEventKind::ScrollDown, 11, 6), m, (10, 5), (80, 24)).unwrap();
        assert_eq!(down, b"\x1b[<65;2;2M".to_vec());
        let right = encode(&ev(MouseEventKind::Down(MouseButton::Right), 11, 6), m, (10, 5), (80, 24)).unwrap();
        assert_eq!(right, b"\x1b[<2;2;2M".to_vec());
    }

    #[test]
    fn legacy_encoding_is_three_bytes_after_the_leader() {
        let m = MouseModes { tracking: true, sgr: false, button_motion: false, any_motion: false };
        let bytes = encode(&ev(MouseEventKind::Down(MouseButton::Left), 11, 6), m, (10, 5), (80, 24))
            .expect("encoded");
        // button 0 + 32, then 1-based x + 32 and y + 32 (so the first cell is 33).
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 32 + 2, 32 + 2]);
    }

    #[test]
    fn nothing_is_forwarded_without_tracking_or_outside_the_pane() {
        let off = MouseModes::default();
        assert!(encode(&ev(MouseEventKind::Down(MouseButton::Left), 11, 6), off, (10, 5), (80, 24)).is_none());

        let on = MouseModes { tracking: true, sgr: true, button_motion: false, any_motion: false };
        assert!(encode(&ev(MouseEventKind::Down(MouseButton::Left), 5, 6), on, (10, 5), (80, 24)).is_none());
        assert!(encode(&ev(MouseEventKind::Down(MouseButton::Left), 11, 30), on, (10, 5), (80, 24)).is_none());
    }
}
