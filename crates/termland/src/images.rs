//! Images as a first-class part of a frame.
//!
//! Cells diff themselves: ratatui compares the new buffer with the previous one
//! and emits only what changed. Images do not live in that grid, so they need
//! their own diff, and getting it wrong is expensive — a 1200×800 wallpaper is
//! ~3.8 MB of pixels, and re-sending it per frame would make the pty the
//! bottleneck of the whole desktop.
//!
//! So the rules are:
//!
//! * a **source** (the pixels) is transmitted once, keyed by a content hash;
//! * a **placement** (where on screen) is created once and then *moved* by
//!   re-sending the same `(image id, placement id)` pair, which kitty replaces
//!   without flicker;
//! * a placement that disappeared is deleted by that same pair, never by
//!   "delete everything", because a pane running `icat` places images of its
//!   own and termland must not wipe them.
//!
//! The z-index matters more than it looks. Kitty draws `z=-1` *above* cell
//! backgrounds, which would paint over the panel. Everything termland places
//! sits at [`Z::BEHIND`] (`i32::MIN`), which is documented as "below cells with
//! non-default background colours" — so the wallpaper shows through the desktop
//! and is covered by every pane that draws a background. Verified against kitty
//! 0.49: `z=-1` covers a red cell background, `z=i32::MIN` does not.

use ratatui::layout::Rect;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Where an image sits relative to text and cell backgrounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Z {
    /// Under the cells. Anything with an explicit background covers it.
    Behind,
    /// Over the cells, under the text.
    Normal,
}

impl Z {
    /// The integer kitty wants. `Behind` is `i32::MIN` for the reason in the
    /// module docs, not because a small negative number would be tidier.
    pub fn kitty(self) -> i32 {
        match self {
            Z::Behind => i32::MIN,
            Z::Normal => 0,
        }
    }
}

/// Decoded pixels, shared so a wallpaper is not copied per frame.
#[derive(Debug, Clone)]
pub struct ImageSource {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Content hash: two frames showing the same wallpaper must not transmit it
    /// twice, and two different wallpapers must not collide.
    pub hash: u64,
}

impl ImageSource {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        width.hash(&mut h);
        height.hash(&mut h);
        rgba.hash(&mut h);
        Self {
            width,
            height,
            rgba,
            hash: h.finish(),
        }
    }

    /// A solid colour — the placeholder a thumbnail slot shows before its
    /// decode finishes, and the cheapest possible test fixture.
    pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let mut px = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            px.extend_from_slice(&rgba);
        }
        Self::new(width, height, px)
    }
}

/// One image on screen, in cells.
#[derive(Debug, Clone)]
pub struct Placement {
    /// Stable identity across frames. Re-using it lets kitty *move* the image
    /// instead of deleting and re-placing it, which is the difference between
    /// smooth scrolling and a stroboscope.
    pub key: u32,
    pub image: Arc<ImageSource>,
    /// Where it goes, in cells of the whole screen.
    pub cells: Rect,
    pub z: Z,
}

impl Placement {
    pub fn new(key: u32, image: Arc<ImageSource>, cells: Rect, z: Z) -> Self {
        Self {
            key,
            image,
            cells,
            z,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    image: u32,
    cells: Rect,
    z: Z,
}

/// Turns per-frame [`Placement`] lists into the smallest set of escape codes
/// that makes the terminal agree. Pure: it returns bytes and touches no tty, so
/// every rule below is unit-tested.
#[derive(Debug, Default)]
pub struct ImageLayer {
    /// Image id for a content hash. Allocated by us, `u32` because kitty's ids
    /// are, and offset into a high band so a child program's ids (which start
    /// at 1) cannot collide with ours.
    by_hash: HashMap<u64, u32>,
    next_id: u32,
    /// What we believe is on screen, by placement key.
    placed: HashMap<u32, Placed>,
}

/// Our ids start here. A TUI that places images uses small ids; keeping to the
/// high half means "delete placement (image, key)" can never name somebody
/// else's image.
const ID_BASE: u32 = 0x4000_0000;

impl ImageLayer {
    pub fn new() -> Self {
        Self {
            by_hash: HashMap::new(),
            next_id: ID_BASE,
            placed: HashMap::new(),
        }
    }

    /// The image id for a source, allocating one on first sight.
    fn id_for(&mut self, src: &ImageSource) -> u32 {
        if let Some(id) = self.by_hash.get(&src.hash) {
            return *id;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.by_hash.insert(src.hash, id);
        id
    }

    /// Forget everything, as if the screen had been cleared. Called on resize:
    /// placements are anchored to the cells they were created in, so after a
    /// reflow they have to be re-made rather than trusted.
    pub fn forget(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let keys: Vec<u32> = self.placed.keys().copied().collect();
        for key in keys {
            if let Some(p) = self.placed.get(&key) {
                delete_placement(&mut out, p.image, key);
            }
        }
        self.placed.clear();
        out
    }

    /// Emit the diff for this frame. Returns the bytes to write to the terminal.
    pub fn frame(&mut self, placements: &[Placement]) -> Vec<u8> {
        let mut out = Vec::new();

        for p in placements {
            if p.cells.width == 0 || p.cells.height == 0 {
                continue;
            }
            let image = self.id_for(&p.image);
            let want = Placed {
                image,
                cells: p.cells,
                z: p.z,
            };

            match self.placed.get(&p.key) {
                Some(have) if *have == want => continue, // already correct
                Some(have) if have.image == image => {
                    // Same picture, new place: re-place under the same
                    // (image, placement) pair, which moves it without flicker.
                    place(&mut out, image, p, true);
                    self.placed.insert(p.key, want);
                }
                _ => {
                    place(&mut out, image, p, true);
                    self.placed.insert(p.key, want);
                }
            }
        }

        // Anything we placed before and did not place now has gone away.
        let alive: std::collections::HashSet<u32> = placements.iter().map(|p| p.key).collect();
        let gone: Vec<u32> = self
            .placed
            .keys()
            .copied()
            .filter(|k| !alive.contains(k))
            .collect();
        for key in gone {
            if let Some(p) = self.placed.remove(&key) {
                delete_placement(&mut out, p.image, key);
            }
        }

        out
    }

    /// How many images the terminal is holding for us — the number that matters
    /// when deciding whether the thumbnail cache is eating memory.
    pub fn sources(&self) -> usize {
        self.by_hash.len()
    }

    pub fn placements(&self) -> usize {
        self.placed.len()
    }
}

/// `a=p` places an already-transmitted image at the cursor. `C=1` keeps the
/// cursor where it was so the cell diff ratatui is about to write is not
/// displaced, and `q=2` silences the acknowledgement — kitty writes replies
/// back into our own stdin, where they would be parsed as keys.
fn place(out: &mut Vec<u8>, image: u32, p: &Placement, keep_cursor: bool) {
    out.extend_from_slice(
        format!(
            "\x1b[{};{}H",
            p.cells.y.saturating_add(1),
            p.cells.x.saturating_add(1)
        )
        .as_bytes(),
    );
    let cursor = if keep_cursor { ",C=1" } else { "" };
    out.extend_from_slice(
        format!(
            "\x1b_Ga=p,i={image},p={},c={},r={},z={}{cursor},q=2\x1b\\",
            p.key,
            p.cells.width,
            p.cells.height,
            p.z.kitty()
        )
        .as_bytes(),
    );
}

fn delete_placement(out: &mut Vec<u8>, image: u32, key: u32) {
    out.extend_from_slice(format!("\x1b_Ga=d,d=i,i={image},p={key},q=2\x1b\\").as_bytes());
}

/// Transmit pixel data for an image: `a=t` (transmit only, no placement),
/// f=32 RGBA, chunked at 4096 base64 bytes as the protocol requires. Quiet
/// (`q=2`) for the same reason as [`place`].
pub fn transmit(out: &mut Vec<u8>, image: u32, src: &ImageSource) {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&src.rgba);
    let bytes = b64.as_bytes();
    let mut first = true;
    let mut off = 0;
    while off < bytes.len() {
        let end = (off + 4096).min(bytes.len());
        let more = if end < bytes.len() { 1 } else { 0 };
        let chunk = &bytes[off..end];
        if first {
            out.extend_from_slice(
                format!(
                    "\x1b_Ga=t,f=32,s={},v={},i={image},m={more},q=2;",
                    src.width, src.height
                )
                .as_bytes(),
            );
            first = false;
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
        off = end;
    }
}

/// Which sources this frame needs transmitted before its placements will show.
/// The layer cannot transmit by itself — the surface owns the wire — so it
/// reports what is missing and then assumes it was done.
pub fn missing(layer: &mut ImageLayer, placements: &[Placement]) -> Vec<(u32, Arc<ImageSource>)> {
    let mut out = Vec::new();
    for p in placements {
        if p.cells.width == 0 || p.cells.height == 0 {
            continue;
        }
        if layer.by_hash.contains_key(&p.image.hash) {
            continue;
        }
        let id = layer.id_for(&p.image);
        out.push((id, p.image.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(n: u8) -> Arc<ImageSource> {
        Arc::new(ImageSource::solid(4, 4, [n, 0, 0, 255]))
    }

    fn tl(s: Arc<ImageSource>, key: u32, x: u16, y: u16) -> Placement {
        Placement::new(key, s, Rect::new(x, y, 10, 4), Z::Behind)
    }

    #[test]
    fn the_same_frame_twice_emits_nothing_the_second_time() {
        let mut l = ImageLayer::new();
        let p = vec![tl(src(1), 1, 0, 0)];
        let _ = missing(&mut l, &p);
        assert!(!l.frame(&p).is_empty());
        assert!(l.frame(&p).is_empty(), "an unchanged frame must be silent");
    }

    #[test]
    fn moving_a_placement_reuses_its_ids_instead_of_deleting_it() {
        let mut l = ImageLayer::new();
        let a = src(1);
        let _ = missing(&mut l, &[tl(a.clone(), 1, 0, 0)]);
        l.frame(&[tl(a.clone(), 1, 0, 0)]);
        let bytes = l.frame(&[tl(a, 1, 5, 3)]);
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("a=p"),
            "a move must place, not delete: {text}"
        );
        assert!(!text.contains("a=d"), "a move must not delete: {text}");
        assert!(
            text.contains("p=1"),
            "the placement id is what makes it a move"
        );
    }

    #[test]
    fn a_placement_that_disappears_is_deleted_by_name() {
        let mut l = ImageLayer::new();
        let a = src(1);
        let _ = missing(&mut l, &[tl(a.clone(), 7, 0, 0)]);
        l.frame(&[tl(a, 7, 0, 0)]);
        let bytes = l.frame(&[]);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("a=d,d=i"), "{text}");
        assert!(
            text.contains("p=7"),
            "deletion must name the placement: {text}"
        );
        // Never a blanket delete: a pane may have placed images of its own.
        assert!(!text.contains("d=A") && !text.contains("d=a,"), "{text}");
    }

    #[test]
    fn an_image_that_is_already_on_screen_is_never_transmitted_twice() {
        let mut l = ImageLayer::new();
        let a = src(1);
        let first = missing(&mut l, &[tl(a.clone(), 1, 0, 0)]);
        assert_eq!(first.len(), 1);
        let second = missing(&mut l, &[tl(a, 2, 0, 0)]);
        assert!(second.is_empty(), "the same pixels must not be re-sent");
        assert_eq!(l.sources(), 1);
    }

    #[test]
    fn transmission_is_chunked_and_quiet() {
        let big = Arc::new(ImageSource::new(64, 64, vec![7u8; 64 * 64 * 4]));
        let mut out = Vec::new();
        transmit(&mut out, 99, &big);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=t,f=32,s=64,v=64,i=99"), "{text}");
        assert!(text.contains("q=2"), "replies must be suppressed: {text}");
        assert!(text.matches("\x1b_G").count() > 1, "should be chunked");
        // Every chunk is a complete escape sequence.
        assert_eq!(text.matches('\x1b').count() % 2, 0);
    }

    #[test]
    fn behind_is_below_cell_backgrounds_and_normal_is_not() {
        assert_eq!(Z::Behind.kitty(), i32::MIN);
        assert!(
            Z::Behind.kitty() < i32::MIN / 2,
            "< INT32_MIN/2 per the spec"
        );
        assert_eq!(Z::Normal.kitty(), 0);
    }

    #[test]
    fn a_zero_sized_placement_is_ignored() {
        let mut l = ImageLayer::new();
        let p = Placement::new(1, src(1), Rect::new(0, 0, 0, 4), Z::Behind);
        assert!(missing(&mut l, std::slice::from_ref(&p)).is_empty());
        assert!(l.frame(&[p]).is_empty());
    }

    #[test]
    fn forgetting_everything_clears_the_screen_but_keeps_the_pixels() {
        let mut l = ImageLayer::new();
        let a = src(1);
        let _ = missing(&mut l, &[tl(a.clone(), 1, 0, 0)]);
        l.frame(&[tl(a.clone(), 1, 0, 0)]);
        let bytes = l.forget();
        assert!(!bytes.is_empty());
        assert_eq!(l.placements(), 0);
        assert_eq!(l.sources(), 1, "the image data is still valid after a move");
    }
}
