//! Surface abstraction — the boundary between termland's core and the thing
//! that actually draws pixels and collects input.
//!
//! v0.1 draws through ratatui into kitty (escape codes into somebody else's
//! terminal). A distro frontend needs to own the screen: either a wgpu surface
//! under `cage`, or direct DRM/KMS. This trait is that boundary, and [`Frame`]
//! is the only drawing surface the core is allowed to know about.
//!
//! The core may never assume:
//! * that drawing means "escape codes into somebody else's terminal"
//! * that an image is an encoded byte blob (the GPU frontend wants a texture
//!   and a rect)
//! * that input arrives as `crossterm::Event`
//! * that the screen is a tty: [`App::draw`](crate::app::App::draw) takes a
//!   [`Frame`], so a headless test can draw a whole UI at 80×24 and read the
//!   cells back.
//!
//! The two halves of the contract are deliberately separate types:
//!
//! ```text
//!   begin_frame() -> Frame        the core draws cells into this
//!   present(Frame)                the surface takes the frame away
//! ```
//!
//! `present` takes the frame **by value** so a GPU/DRM surface can consume the
//! buffer instead of copying it. [`KittySurface`] copies it into ratatui's own
//! buffer so ratatui's diffing still works, which is why the kitty path pays
//! one memcpy per frame that a tier-2 frontend will not.

use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use ratatui::widgets::{StatefulWidget, Widget};
use crate::images::{ImageLayer, Placement};
use crate::widgets::Pointer;
use ratatui::Terminal;
use std::fmt::Debug;
use std::io::{self, Stdout, Write};

/// A frame of output: the core draws cells into [`Frame::buffer_mut`] (or with
/// [`Frame::render`] / [`Frame::render_stateful`]), and hands the whole frame
/// back through [`Surface::present`].
///
/// A frame owns its buffer. That costs one allocation and one `Buffer::empty`
/// per redraw; a tier-2 frontend with a frame clock will want a reusable
/// back buffer instead, which is a change to this type and nothing else.
pub struct Frame {
    area: Rect,
    buf: Buffer,
    cursor: Option<(u16, u16)>,
    /// Images on top of (or behind) the cells. The buffer diffs itself; these
    /// do not live in the grid, so the surface diffs them against the last
    /// frame instead — see [`crate::images`].
    images: Vec<Placement>,
}

impl Frame {
    pub fn new(area: Rect) -> Self {
        Self {
            area,
            buf: Buffer::empty(area),
            cursor: None,
            images: Vec::new(),
        }
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buf
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buf
    }

    /// Draw a widget into this frame. The core-side equivalent of
    /// `ratatui::Frame::render_widget`, minus the terminal.
    pub fn render<W: Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, &mut self.buf);
    }

    /// Draw a stateful widget (images) into this frame.
    pub fn render_stateful<W: StatefulWidget>(&mut self, widget: W, area: Rect, state: &mut W::State) {
        widget.render(area, &mut self.buf, state);
    }

    /// Place the text cursor. The surface decides whether that means
    /// `ESC[<row>;<col>H` (kitty) or a blinking quad in a GPU atlas.
    pub fn set_cursor_position(&mut self, pos: (u16, u16)) {
        self.cursor = Some(pos);
    }

    pub fn cursor(&self) -> Option<(u16, u16)> {
        self.cursor
    }

    /// Put an image on screen at a cell rect. A placement whose `key` repeats
    /// in the next frame is *moved*, not re-transmitted.
    pub fn place(&mut self, placement: Placement) {
        self.images.push(placement);
    }

    pub fn images(&self) -> &[Placement] {
        &self.images
    }

    /// Blank the frame with a style — cheap full repaint, used between frames
    /// by surfaces that do not double-buffer.
    pub fn clear(&mut self) {
        self.buf.reset();
        self.cursor = None;
        self.images.clear();
    }
}

/// The surface trait: everything termland needs from the host to draw and
/// receive input. Implemented today by [`KittySurface`] (ratatui → escape
/// codes), tomorrow by a wgpu/DRM surface.
pub trait Surface: Debug {
    /// The full screen size in cells. The core subtracts its own chrome.
    fn size(&self) -> Rect;

    /// Begin a frame covering the whole screen.
    fn begin_frame(&mut self) -> Frame;

    /// Present a completed frame.
    fn present(&mut self, frame: Frame);

    /// Set the window title.
    fn set_title(&mut self, _title: &str) {}

    /// Ask the host for a pointer shape. A surface that cannot do it ignores
    /// the request: the shape is a courtesy, never information.
    fn set_pointer(&mut self, _shape: Pointer) {}

    /// Report a status message (shown in the surface's chrome, if any).
    fn status(&mut self, _msg: &str) {}

    /// Handle an input event. Returns `true` if the event was consumed.
    fn input(&mut self, _event: InputEvent) -> bool {
        false
    }

    /// Release the host: leave raw mode, drop the alt screen, hand the seat
    /// back. Must be idempotent — it is called both explicitly and on drop,
    /// because a panic through a `Drop` is not something a login screen can
    /// afford.
    fn restore(&mut self) {}
}

/// An input event from the host: key, mouse, paste, or resize.
///
/// This is the shape the core consumes. A DRM frontend feeds it from
/// `libinput`; the kitty surface feeds it from crossterm. `Key` carries a
/// crossterm key event for now, which is the one remaining leak and is small
/// enough to be worth closing with the tier-2 frontend rather than today.
#[derive(Clone, Debug)]
pub enum InputEvent {
    Key(ratatui::crossterm::event::KeyEvent),
    Mouse(ratatui::crossterm::event::MouseEvent),
    Paste(String),
    Resize(u16, u16),
}

/// The kitty surface: today's implementation. Draws through ratatui into the
/// host terminal using escape codes. Input arrives as `crossterm::Event`.
#[derive(Debug)]
pub struct KittySurface {
    area: Rect,
    /// `None` when detached — a headless surface used by tests, which must not
    /// touch the tty.
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
    restored: bool,
    /// Image placements, diffed across frames so a wallpaper costs one
    /// transmission and one placement command, not one per frame.
    images: ImageLayer,
    /// Last shape written, so a hover that never moves does not emit an escape
    /// sequence on every frame.
    pointer: Pointer,
}

impl KittySurface {
    /// A surface with no terminal attached: it knows the size and draws into
    /// frames, but never writes to stdout. This is what tests use.
    pub fn new() -> Result<Self, io::Error> {
        let (w, h) = ratatui::crossterm::terminal::size()?;
        Ok(Self {
            area: Rect::new(0, 0, w, h),
            terminal: None,
            restored: false,
            images: ImageLayer::new(),
            pointer: Pointer::Default,
        })
    }

    /// A surface for a real terminal: raw mode, alt screen, mouse capture,
    /// bracketed paste and the kitty keyboard protocol, in that order.
    ///
    /// Call this **before** asking the terminal about its graphics support —
    /// the reply has to arrive after raw mode is on or it sits in the cooked
    /// line buffer and the query hangs.
    pub fn open() -> Result<Self, io::Error> {
        let terminal = ratatui::init();
        let sz = terminal.size()?;
        let area = Rect::new(0, 0, sz.width, sz.height);
        let _ = execute!(
            io::stdout(),
            EnableBracketedPaste,
            EnableMouseCapture,
            PushKeyboardEnhancementFlags(
                // DISAMBIGUATE makes ctrl/alt unambiguous; REPORT_ALL_KEYS is
                // what lets a *Super* chord arrive at all — without it the
                // terminal cannot represent the modifier and the desktop key
                // silently disappears.
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
            ),
        );
        Ok(Self {
            area,
            terminal: Some(terminal),
            restored: false,
            images: ImageLayer::new(),
            pointer: Pointer::Default,
        })
    }

    /// Re-read the terminal size. Called on `SIGWINCH`-shaped resizes; the
    /// core also notices a size change on its next frame, so this is belt and
    /// braces.
    pub fn sync_size(&mut self) -> io::Result<()> {
        if let Some(t) = self.terminal.as_mut() {
            self.area = Rect::new(0, 0, t.size()?.width, t.size()?.height);
        }
        Ok(())
    }

    pub fn is_attached(&self) -> bool {
        self.terminal.is_some()
    }
}

impl Surface for KittySurface {
    fn size(&self) -> Rect {
        self.area
    }

    fn begin_frame(&mut self) -> Frame {
        Frame::new(self.area)
    }

    fn present(&mut self, frame: Frame) {
        let Some(term) = self.terminal.as_mut() else {
            return;
        };
        let cursor = frame.cursor();
        let src = frame.buffer();
        let src_area = src.area;
        // ratatui diffs against its own buffer, so the cells have to be copied
        // in rather than handed over. A tier-2 surface consumes the frame.
        let _ = term.draw(|f| {
            let dst = f.buffer_mut();
            let w = src_area.width.min(dst.area.width);
            let h = src_area.height.min(dst.area.height);
            for y in 0..h {
                for x in 0..w {
                    dst[(x, y)] = src[(x, y)].clone();
                }
            }
            if let Some((cx, cy)) = cursor {
                if cx < dst.area.width && cy < dst.area.height {
                    f.set_cursor_position((cx, cy));
                }
            }
        });

        // Images go out *after* the cells. The cell diff is what the terminal
        // has to have before an image lands in the right place, and the image
        // commands are anchored to cells, not pixels.
        let new_area = Rect::new(0, 0, src_area.width, src_area.height);
        if new_area != self.area {
            // Placements were anchored to the old grid; drop and re-make them
            // rather than leave the wallpaper half off the screen.
            self.area = new_area;
            let mut out = self.images.forget();
            let mut stdout = io::stdout();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
            out.clear();
        }

        let placements = frame.images();
        let mut out = Vec::new();
        for (id, src) in crate::images::missing(&mut self.images, placements) {
            crate::images::transmit(&mut out, id, &src);
        }
        out.extend_from_slice(&self.images.frame(placements));
        if !out.is_empty() {
            let mut stdout = io::stdout();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
        }
    }

    fn set_title(&mut self, title: &str) {
        let _ = execute!(io::stdout(), ratatui::crossterm::terminal::SetTitle(title));
    }

    fn set_pointer(&mut self, shape: Pointer) {
        if shape == self.pointer || self.terminal.is_none() {
            return;
        }
        self.pointer = shape;
        let mut out = io::stdout();
        let _ = out.write_all(shape.escape().as_bytes());
        let _ = out.flush();
    }

    fn restore(&mut self) {
        if self.restored || self.terminal.is_none() {
            return;
        }
        self.restored = true;
        // Images live outside the alt screen's cell buffer, so leaving without
        // deleting them leaves ghosts on some terminals.
        let _ = io::stdout().write_all(&self.images.forget());
        let _ = io::stdout().flush();
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            DisableMouseCapture,
        );
        ratatui::restore();
    }
}

impl Drop for KittySurface {
    fn drop(&mut self) {
        self.restore();
    }
}

impl Default for KittySurface {
    fn default() -> Self {
        Self::new().unwrap_or(Self {
            area: Rect::new(0, 0, 80, 24),
            terminal: None,
            restored: false,
            images: ImageLayer::new(),
            pointer: Pointer::Default,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::Line;

    #[test]
    fn kitty_surface_default_size() {
        let s = KittySurface::default();
        // A headless surface reports the whole screen; the core takes its own
        // panel and status rows off that, so nothing is subtracted here.
        assert!(s.size().width > 0);
        assert!(s.size().height > 0);
        assert!(!s.is_attached());
    }

    #[test]
    fn a_frame_starts_blank_and_covers_the_area() {
        let f = Frame::new(Rect::new(0, 0, 10, 3));
        assert_eq!(f.area(), Rect::new(0, 0, 10, 3));
        assert_eq!(f.buffer().area, Rect::new(0, 0, 10, 3));
        assert!(f.cursor().is_none());
        for x in 0..10 {
            for y in 0..3 {
                assert_eq!(f.buffer()[(x, y)].symbol(), " ");
            }
        }
    }

    #[test]
    fn render_puts_a_widget_in_the_buffer() {
        let mut f = Frame::new(Rect::new(0, 0, 12, 2));
        f.render(ratatui::widgets::Paragraph::new(Line::from("hello")), f.area());
        let text: String = (0..12).map(|x| f.buffer()[(x, 0)].symbol()).collect();
        assert!(text.starts_with("hello"), "got {text:?}");
    }

    #[test]
    fn styles_reach_the_buffer() {
        let mut f = Frame::new(Rect::new(0, 0, 4, 1));
        f.render(
            ratatui::widgets::Paragraph::new("ab").style(Style::default().fg(ratatui::style::Color::Red)),
            f.area(),
        );
        assert_eq!(f.buffer()[(0, 0)].fg, ratatui::style::Color::Red);
        assert_eq!(f.buffer()[(0, 0)].symbol(), "a");
    }

    #[test]
    fn cursor_is_only_where_it_was_put() {
        let mut f = Frame::new(Rect::new(0, 0, 8, 2));
        assert_eq!(f.cursor(), None);
        f.set_cursor_position((3, 1));
        assert_eq!(f.cursor(), Some((3, 1)));
    }

    #[test]
    fn present_on_a_detached_surface_is_a_no_op() {
        // Nothing to assert about stdout here: the point is that presenting
        // without a terminal must not panic, because every headless render
        // test in the crate goes through it.
        let mut s = KittySurface::default();
        let mut f = s.begin_frame();
        f.render(ratatui::widgets::Paragraph::new("x"), f.area());
        s.present(f);
        s.restore();
    }
}
