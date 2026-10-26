//! One process in one pty, plus its `vt100` screen.
//!
//! The reader half runs on its own thread and pushes raw bytes into the main
//! loop's channel; the parser is only ever touched from the main thread, so
//! there is no lock anywhere in the hot path.

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::sync::mpsc::Sender;

use crate::layout::PaneId;
use crate::mouse::{MouseModes, MouseSniffer};
use crate::msg::Msg;

const SCROLLBACK: usize = 2000;

/// `vt100` 0.18 takes `NonZeroU16` for geometry, because a zero-sized screen is
/// meaningless. Clamp rather than unwrap: a pane can genuinely be 0 rows tall
/// for one frame while a split settles.
fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap_or(NonZeroU16::MIN)
}

pub struct TermPane {
    pub label: String,
    pub cmdline: String,
    /// Set by OSC 0/2 in the child's output, which is how shells and editors
    /// announce themselves. `vt100` 0.18 does not expose the title, so it is
    /// sniffed out of the byte stream here.
    pub osc_title: String,
    parser: vt100::Parser,
    sniffer: TitleSniffer,
    mouse_sniffer: MouseSniffer,
    query_sniffer: QuerySniffer,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    pub size: (u16, u16), // rows, cols
    pub exited: bool,
    pub bytes_in: u64,
    pub scroll_offset: u16,
    /// Whether the child asked for mouse reporting, and in which encoding.
    pub mouse: MouseModes,
}

impl TermPane {
    pub fn spawn(
        id: PaneId,
        size: (u16, u16),
        argv: Vec<String>,
        tx: Sender<Msg>,
    ) -> Result<Self> {
        let (rows, cols) = (size.0.max(1), size.1.max(1));
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        let prog = argv.first().cloned().unwrap_or_else(default_shell);
        let mut cb = CommandBuilder::new(&prog);
        for a in argv.iter().skip(1) {
            cb.arg(a);
        }
        cb.env("TERM", "xterm-256color");
        cb.env("COLORTERM", "truecolor");
        cb.env("TERMLAND", "1");
        cb.env("TERMLAND_PANE", id.to_string());
        if let Ok(cwd) = std::env::current_dir() {
            cb.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(cb)
            .with_context(|| format!("spawn {prog}"))?;
        // The slave must be dropped in the parent, or the child never sees EOF.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("pty reader")?;
        let writer = pair.master.take_writer().context("pty writer")?;

        std::thread::Builder::new()
            .name(format!("pty-{id}"))
            .spawn(move || {
                let mut buf = [0u8; 16384];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send(Msg::Output(id, buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = tx.send(Msg::Exited(id));
            })
            .context("spawn pty reader thread")?;

        // Reaping is the child handle's job; keep it alive for the pane's life.
        std::thread::Builder::new()
            .name(format!("reap-{id}"))
            .spawn(move || {
                let mut child = child;
                let _ = child.wait();
            })
            .ok();

        let label = argv
            .first()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.clone())
            })
            .unwrap_or_else(|| "shell".into());

        Ok(Self {
            label,
            cmdline: argv.join(" "),
            osc_title: String::new(),
            parser: vt100::Parser::new(nz(rows), nz(cols), SCROLLBACK),
            sniffer: TitleSniffer::default(),
            mouse_sniffer: MouseSniffer::default(),
            query_sniffer: QuerySniffer::default(),
            master: pair.master,
            writer,
            size: (rows, cols),
            exited: false,
            bytes_in: 0,
            scroll_offset: 0,
            mouse: MouseModes::default(),
        })
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.bytes_in += bytes.len() as u64;
        self.parser.process(bytes);
        if let Some(t) = self.sniffer.feed(bytes) {
            self.osc_title = t;
        }
        let mut modes = self.mouse;
        self.mouse_sniffer.feed(bytes, &mut modes);
        self.mouse = modes;

        // A terminal that does not answer `CSI 6 n` is a terminal programs
        // give up on: bluetui prints "the cursor position could not be read"
        // and refuses to draw. Answer the queries xterm answers.
        let cursor = self.parser.screen().cursor_position();
        let replies = self.query_sniffer.feed(bytes, cursor);
        if !replies.is_empty() && !self.exited {
            let _ = self.writer.write_all(&replies);
            let _ = self.writer.flush();
        }
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// What the child calls itself, else the program name we spawned.
    pub fn title(&self) -> String {
        if self.osc_title.is_empty() {
            self.label.clone()
        } else {
            self.osc_title.clone()
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        self.parser.set_size(nz(rows), nz(cols));
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if self.exited {
            return;
        }
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Adjust the scrollback viewport. Positive scrolls down (toward newer
    /// output), negative scrolls up (toward older output). The offset is
    /// stored on the pane; the renderer uses it when drawing.
    pub fn scroll(&mut self, delta: i16) {
        let max_scroll = self.size.0.saturating_sub(1);
        let new = (self.scroll_offset as i32 + delta as i32)
            .clamp(0, max_scroll as i32) as u16;
        self.scroll_offset = new;
    }
}

pub fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
}

pub fn shell_argv() -> Vec<String> {
    vec![default_shell()]
}

/// Answers the handful of device queries a full-screen program asks the
/// terminal *it* thinks it is talking to.
///
/// `vt100` parses output; it does not reply. Without replies, `CSI 6 n` (where
/// is the cursor?) hangs until the program's timeout and the result is the kind
/// of `Error: the cursor position could not be read` banner that bluetui draws.
///
/// Only complete sequences are answered, and an incomplete one is carried into
/// the next read, so a query split across two pty chunks is answered exactly
/// once.
#[derive(Default)]
pub struct QuerySniffer {
    pending: Vec<u8>,
}

impl QuerySniffer {
    pub fn feed(&mut self, bytes: &[u8], cursor: (u16, u16)) -> Vec<u8> {
        const PATTERNS: [&[u8]; 5] = [b"\x1b[6n", b"\x1b[5n", b"\x1b[c", b"\x1b[>c", b"\x1b[?c"];

        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut i = 0;
        while i + 3 <= self.pending.len() {
            let hit = PATTERNS
                .iter()
                .find(|p| self.pending[i..].starts_with(p))
                .copied();
            match hit {
                Some(p) => {
                    match p {
                        // CPR: 1-based row;column.
                        b"\x1b[6n" => out.extend_from_slice(
                            format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).as_bytes(),
                        ),
                        b"\x1b[5n" => out.extend_from_slice(b"\x1b[0n"),
                        // Primary device attributes: a VT220 with colour.
                        b"\x1b[c" | b"\x1b[?c" => out.extend_from_slice(b"\x1b[?62;22c"),
                        _ => out.extend_from_slice(b"\x1b[>41;411;0c"),
                    }
                    i += p.len();
                }
                None => i += 1,
            }
        }

        // Keep only a suffix that could still become a query; anything else is
        // ordinary output and must not be re-scanned.
        let len = self.pending.len();
        let mut keep = 0;
        for n in 1..=3usize {
            if len < n {
                break;
            }
            let suffix = &self.pending[len - n..];
            if PATTERNS
                .iter()
                .any(|p| p.len() > n && p.starts_with(suffix))
            {
                keep = n;
            }
        }
        self.pending.drain(..len - keep);
        out
    }
}

/// Pulls `OSC 0 ; text BEL` and `OSC 2 ; text ST` out of a byte stream.
///
/// The pane header is the only label a window gets in a tiling WM, so real
/// titles matter — but `vt100` 0.18 does not surface them. Only a short tail is
/// kept between calls: enough to reassemble a sequence split across two reads,
/// bounded to nothing otherwise.
#[derive(Default)]
pub struct TitleSniffer {
    tail: Vec<u8>,
}

impl TitleSniffer {
    pub fn feed(&mut self, bytes: &[u8]) -> Option<String> {
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > 8192 {
            let drop = self.tail.len() - 1024;
            self.tail.drain(..drop);
        }

        let mut latest = None;
        let mut keep_from = self.tail.len().saturating_sub(16);
        let mut i = 0;
        while i + 1 < self.tail.len() {
            if self.tail[i] == 0x1b && self.tail[i + 1] == b']' {
                match parse_osc(&self.tail, i) {
                    Some((kind, text, next)) => {
                        if kind == 0 || kind == 2 {
                            latest = Some(text);
                        }
                        i = next;
                        continue;
                    }
                    None => {
                        // Incomplete (or not a numeric OSC): wait for more bytes.
                        keep_from = i;
                        break;
                    }
                }
            }
            i += 1;
        }
        keep_from = keep_from.min(self.tail.len());
        self.tail.drain(..keep_from);
        latest
    }
}

/// `(number, payload, index after the terminator)`, or `None` if the sequence
/// is not complete yet.
fn parse_osc(buf: &[u8], i: usize) -> Option<(u32, String, usize)> {
    let mut j = i + 2;
    let mut kind: u32 = 0;
    let mut digits = 0;
    while j < buf.len() && buf[j].is_ascii_digit() {
        kind = kind.saturating_mul(10).saturating_add((buf[j] - b'0') as u32);
        j += 1;
        digits += 1;
    }
    if digits == 0 || j >= buf.len() || buf[j] != b';' {
        return None;
    }
    let start = j + 1;
    let mut k = start;
    while k < buf.len() {
        if buf[k] == 0x07 {
            return Some((kind, lossy(&buf[start..k]), k + 1));
        }
        if buf[k] == 0x1b && k + 1 < buf.len() && buf[k + 1] == b'\\' {
            return Some((kind, lossy(&buf[start..k]), k + 2));
        }
        k += 1;
    }
    None
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_bel_terminated_title() {
        let mut s = TitleSniffer::default();
        assert_eq!(s.feed(b"\x1b]0;bash\x07hello"), Some("bash".into()));
    }

    #[test]
    fn reads_an_st_terminated_title() {
        let mut s = TitleSniffer::default();
        assert_eq!(s.feed(b"\x1b]2;~/dev\x1b\\"), Some("~/dev".into()));
    }

    #[test]
    fn reassembles_a_title_split_across_reads() {
        let mut s = TitleSniffer::default();
        assert_eq!(s.feed(b"junk\x1b]0;car"), None);
        assert_eq!(s.feed(b"go\x07 more"), Some("cargo".into()));
    }

    #[test]
    fn hyperlinks_are_not_titles() {
        let mut s = TitleSniffer::default();
        let link = b"\x1b]8;;https://example.com\x1b\\text\x1b]8;;\x1b\\";
        assert_eq!(s.feed(link), None);
    }

    #[test]
    fn plain_output_yields_nothing_and_does_not_grow() {
        let mut s = TitleSniffer::default();
        for _ in 0..500 {
            assert_eq!(s.feed(b"a plain line of program output, nothing to see\r\n"), None);
        }
        assert!(s.tail.len() < 64, "tail must stay bounded, got {}", s.tail.len());
    }

    #[test]
    fn the_last_title_wins() {
        let mut s = TitleSniffer::default();
        assert_eq!(s.feed(b"\x1b]0;one\x07\x1b]0;two\x07"), Some("two".into()));
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;

    #[test]
    fn a_cursor_position_query_is_answered_one_based() {
        let mut q = QuerySniffer::default();
        let out = q.feed(b"\x1b[6n", (4, 9));
        assert_eq!(out, b"\x1b[5;10R".to_vec());
    }

    #[test]
    fn a_query_split_across_reads_is_answered_once() {
        let mut q = QuerySniffer::default();
        assert!(q.feed(b"logo\x1b[6", (1, 1)).is_empty());
        let out = q.feed(b"n done", (1, 1));
        assert_eq!(out, b"\x1b[2;2R".to_vec());
    }

    #[test]
    fn plain_text_is_never_mistaken_for_a_query() {
        let mut q = QuerySniffer::default();
        assert_eq!(
            q.feed(b"\x1b[6n but \x1b[1;2H is a move", (0, 0)),
            b"\x1b[1;1R".to_vec()
        );
        assert!(q.feed(b"no escapes at all", (0, 0)).is_empty());
        assert!(q.feed(b"0123456789abcdef", (0, 0)).is_empty());
    }

    #[test]
    fn device_attributes_and_status_report_get_an_answer() {
        let mut q = QuerySniffer::default();
        assert_eq!(q.feed(b"\x1b[c", (0, 0)), b"\x1b[?62;22c".to_vec());
        assert_eq!(q.feed(b"\x1b[5n", (0, 0)), b"\x1b[0n".to_vec());
    }
}
