//! Immediate-mode widgets: identity, hover, press, and the animation that makes
//! them feel like a desktop rather than a form.
//!
//! There is no widget tree and no retained state per control. A control is a
//! rect plus an [`Id`]; the app calls [`Ui::hit`] while it draws, in draw order,
//! and the last one that contains the pointer wins — which is exactly "topmost
//! wins" without anyone having to maintain a z-order.
//!
//! Why ids at all: hover and press have to survive layout changes and be
//! animated across frames. A raw `Rect` cannot do that (it moves), and an index
//! into a list renumbers when the list scrolls. An id derived from *what the
//! control is* (`"file-row"`, `"panel-wifi"`, `("menu", "Files")`) is stable
//! through both.
//!
//! Click semantics are the desktop's, not the web's: a press arms an id, a
//! release over that same id is a click. Dragging off cancels. That is why a
//! click that starts on one row and ends on another does nothing.

use crate::anim::Tween;
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

/// The pointer shape to ask the terminal for. Kitty honours these (`OSC 22`);
/// a terminal that does not simply keeps its arrow, which is why the app never
/// *depends* on the shape to communicate anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pointer {
    #[default]
    Default,
    /// Over something clickable: buttons, rows, icons, the close box.
    Pointer,
    /// Over the terminal grid, where a click places a text cursor.
    Text,
    /// Over a vertical divider.
    ResizeHorizontal,
    /// Over a horizontal divider.
    ResizeVertical,
    /// A drag is in progress with no better shape to offer.
    Grabbing,
}

impl Pointer {
    /// The name kitty expects in `OSC 22 ; <name> ST`.
    pub fn name(self) -> &'static str {
        match self {
            Pointer::Default => "default",
            Pointer::Pointer => "pointer",
            Pointer::Text => "text",
            Pointer::ResizeHorizontal => "ew-resize",
            Pointer::ResizeVertical => "ns-resize",
            Pointer::Grabbing => "grabbing",
        }
    }

    /// The escape sequence, or empty for the shape we start in.
    pub fn escape(self) -> String {
        format!("\x1b]22;{}\x1b\\", self.name())
    }
}

/// A stable identity for a control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id(u64);

impl Id {
    /// An id from a namespace and a key. Two different lists can both have a
    /// row 3 as long as they do not share a scope.
    pub fn new(scope: &str, key: impl Hash) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        scope.hash(&mut h);
        key.hash(&mut h);
        Self(h.finish())
    }

    /// An id from a plain string — the common case for a button.
    pub fn of(name: &str) -> Self {
        Self::new("", name)
    }
}

/// Pointer and control state for one frame at a time.
#[derive(Debug, Default)]
pub struct Ui {
    pointer: Option<(u16, u16)>,
    /// The control the pointer is over, as of the *last* completed frame.
    hover: Option<Id>,
    /// The control the pointer was over while this frame was drawn.
    candidate: Option<Id>,
    /// The control a press started on, still held.
    pressed: Option<Id>,
    fades: HashMap<Id, Tween>,
    pointer_shape: Pointer,
}

impl Ui {
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the pointer is. `None` when it has left the window, which is the
    /// signal to drop every hover.
    pub fn set_pointer(&mut self, pos: Option<(u16, u16)>) {
        self.pointer = pos;
    }

    pub fn pointer(&self) -> Option<(u16, u16)> {
        self.pointer
    }

    pub fn hover(&self) -> Option<Id> {
        self.hover
    }

    /// Start a frame: forget this frame's hover candidate. Every frame
    /// recomputes hover from geometry, so a row that scrolls away stops being
    /// hovered without anything having to remember to clear it.
    pub fn begin_frame(&mut self) {
        self.candidate = None;
    }

    /// Register a control and report whether the pointer is inside it *now*.
    /// Call in draw order; the last registration containing the pointer wins.
    pub fn hit(&mut self, id: Id, rect: Rect) -> bool {
        let inside = match self.pointer {
            Some((x, y)) => {
                x >= rect.x
                    && y >= rect.y
                    && x < rect.x.saturating_add(rect.width)
                    && y < rect.y.saturating_add(rect.height)
            }
            None => false,
        };
        if inside {
            self.candidate = Some(id);
        }
        inside
    }

    /// Commit the frame's hover. Returns true if it changed, which is when the
    /// loop must switch to the fast clock.
    ///
    /// The fades are started *here* rather than when a widget asks for its
    /// amount: a control that stops being hovered may not be drawn by the next
    /// frame (a menu closed, a row scrolled out), and its highlight still has
    /// to fade out instead of vanishing.
    pub fn end_frame(&mut self) -> bool {
        if self.candidate == self.hover {
            return false;
        }
        let old = self.hover;
        self.hover = self.candidate;
        if let Some(id) = old {
            self.start_fade(id, 0.0);
        }
        if let Some(id) = self.candidate {
            self.start_fade(id, 1.0);
        }
        true
    }

    fn start_fade(&mut self, id: Id, target: f32) {
        let from = if self.fades.contains_key(&id) {
            0.0
        } else {
            1.0 - target
        };
        let t = self.fades.entry(id).or_insert_with(|| Tween::settled(from));
        t.to(target, Duration::from_millis(110));
    }

    /// A press landed at the current pointer position, over `id`.
    pub fn press(&mut self, id: Id) {
        self.pressed = Some(id);
    }

    /// The button came up: a click only if it is released over the same control
    /// that it was pressed on.
    pub fn release(&mut self) -> Option<Id> {
        let pressed = self.pressed.take()?;
        if self.candidate == Some(pressed) {
            Some(pressed)
        } else {
            None
        }
    }

    /// Abandon a press without clicking (a drag started, focus was lost).
    pub fn cancel_press(&mut self) {
        self.pressed = None;
    }

    pub fn pressed(&self) -> Option<Id> {
        self.pressed
    }

    /// Animated hover amount, 0.0 → 1.0, for a control. Call it while drawing;
    /// the tween is started on the first call for a given id.
    pub fn fade(&mut self, id: Id, _dt: f32) -> f32 {
        let Some(t) = self.fades.get(&id) else {
            return if self.hover == Some(id) { 1.0 } else { 0.0 };
        };
        let v = t.value();
        let settled_zero = v == 0.0 && !t.running();
        if settled_zero {
            // Retire settled tweens at 0 so a long session with many rows does
            // not accumulate one entry per row ever hovered.
            self.fades.remove(&id);
        }
        v
    }

    /// How many controls currently have a running animation — the loop uses
    /// this to decide between 60 fps and a slow idle heartbeat.
    pub fn animating(&self) -> bool {
        self.fades.values().any(|t| t.running())
    }

    /// The pointer shape the app decided on this frame.
    pub fn set_pointer_shape(&mut self, shape: Pointer) {
        self.pointer_shape = shape;
    }

    pub fn pointer_shape(&self) -> Pointer {
        self.pointer_shape
    }

    /// Decide the pointer shape from what is under it. Called by the app after
    /// drawing, so "over a button" is decided in exactly one place.
    pub fn resolve_pointer(&mut self, shape: Pointer) {
        self.pointer_shape = shape;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn the_topmost_registration_wins() {
        let mut ui = Ui::new();
        ui.set_pointer(Some((5, 5)));
        ui.begin_frame();
        assert!(ui.hit(Id::of("under"), rect(0, 0, 20, 20)));
        assert!(ui.hit(Id::of("over"), rect(2, 2, 10, 10)));
        ui.end_frame();
        // Drawn last is on top, so the smaller rect wins.
        assert_eq!(ui.hover(), Some(Id::of("over")));
    }

    #[test]
    fn hover_does_not_stick_when_the_row_moves_away() {
        let mut ui = Ui::new();
        ui.set_pointer(Some((5, 5)));
        ui.begin_frame();
        ui.hit(Id::of("row"), rect(0, 0, 20, 6));
        ui.end_frame();
        assert_eq!(ui.hover(), Some(Id::of("row")));

        // Next frame the row is elsewhere: hover must be recomputed, not kept.
        ui.begin_frame();
        ui.hit(Id::of("row"), rect(0, 10, 20, 6));
        ui.end_frame();
        assert_eq!(ui.hover(), None);
    }

    #[test]
    fn a_click_needs_press_and_release_on_the_same_control() {
        let mut ui = Ui::new();
        ui.set_pointer(Some((3, 3)));
        ui.begin_frame();
        ui.hit(Id::of("a"), rect(0, 0, 10, 10));
        ui.press(Id::of("a"));

        // Released over a different control: no click.
        ui.set_pointer(Some((30, 30)));
        ui.begin_frame();
        ui.hit(Id::of("b"), rect(20, 20, 10, 10));
        assert_eq!(ui.release(), None);

        // Pressed and released on the same one: a click.
        ui.press(Id::of("a"));
        ui.set_pointer(Some((3, 3)));
        ui.begin_frame();
        ui.hit(Id::of("a"), rect(0, 0, 10, 10));
        assert_eq!(ui.release(), Some(Id::of("a")));
    }

    #[test]
    fn a_hover_fade_runs_then_settles() {
        let mut ui = Ui::new();
        ui.set_pointer(Some((1, 1)));
        ui.begin_frame();
        ui.hit(Id::of("x"), rect(0, 0, 5, 5));
        ui.end_frame();
        assert!(ui.animating(), "a hover change must start an animation");
        for _ in 0..120 {
            ui.fade(Id::of("x"), 1.0 / 60.0);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(ui.fade(Id::of("x"), 0.0) > 0.9, "fade should reach ~1");
    }

    #[test]
    fn an_idle_ui_is_not_animating() {
        let ui = Ui::new();
        assert!(!ui.animating());
    }

    #[test]
    fn ids_are_stable_and_scoped() {
        assert_eq!(Id::of("open"), Id::of("open"));
        assert_ne!(Id::of("open"), Id::of("close"));
        assert_ne!(Id::new("files", 3u32), Id::new("menu", 3u32));
    }

    #[test]
    fn pointer_shapes_use_the_names_the_protocol_defines() {
        assert_eq!(Pointer::Pointer.escape(), "\x1b]22;pointer\x1b\\");
        assert_eq!(Pointer::Text.name(), "text");
        assert_eq!(Pointer::ResizeHorizontal.name(), "ew-resize");
        assert_eq!(Pointer::Default.name(), "default");
    }
}
