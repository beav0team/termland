//! The wallpaper: the picture behind the desktop icons, if one was chosen.
//!
//! A terminal draws characters, so a picture arrives as one cell per character
//! — which is a very coarse picture. Two things make it look like a wallpaper
//! anyway:
//!
//! * **half blocks.** `▀` puts one colour in the foreground (the top half of
//!   the cell) and another in the background (the bottom half), so every cell
//!   carries two pixels and the vertical resolution doubles for free. A 100×50
//!   area becomes a 200-pixel-tall picture instead of a 50-character one.
//! * **dimming.** A photograph at full strength fights the icon names printed
//!   on top of it; every colour is pulled toward the pane colour on the way in,
//!   so the picture stays recognisable and the text stays readable.
//!
//! The choice is remembered in `~/.config/termland/wallpaper.path` — a plain
//! one-line file, so setting it from a script is `echo` and not a parser. No
//! picture is copied anywhere: the file is read where it lies.
//!
//! The gradient in [`crate::ui::wallpaper_colour`] is still the default. A
//! session with no picture is the normal case, and it costs nothing.

use image::imageops::FilterType;
use image::{DynamicImage, Rgb};
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::fs;
use std::path::{Path, PathBuf};

use crate::theme;

/// The remembered choice, relative to the config directory.
const CHOICE: &str = "wallpaper.path";

/// How far each pixel is pulled toward the pane colour. High enough that white
/// icon names read over a white sky, low enough that you can still tell what
/// the picture is.
const DIM: f32 = 0.34;

pub struct Wallpaper {
    /// The file the session was told to use. `None` means "no wallpaper was
    /// chosen", which is not an error — the gradient answers for it.
    pub path: Option<PathBuf>,
    /// Where the choice is written. A field, not a constant, so a test can
    /// point it at a temp directory instead of the user's real config.
    choice: PathBuf,
    source: Option<DynamicImage>,
    /// One entry per cell of `size`, row-major: `(top half, bottom half)`.
    cells: Vec<(Color, Color)>,
    size: (u16, u16),
}

impl Default for Wallpaper {
    fn default() -> Self {
        Self::load()
    }
}

impl Wallpaper {
    /// Load the remembered choice, if there is one and it still decodes.
    ///
    /// A missing file, a deleted picture and a corrupt one all end the same
    /// way: no wallpaper, gradient instead, session starts. A desktop that
    /// refuses to boot because a JPEG is truncated is not a desktop.
    pub fn load() -> Self {
        Self::from_choice(Self::choice_file())
    }

    pub fn from_choice(choice: PathBuf) -> Self {
        let mut w = Self {
            path: None,
            choice,
            source: None,
            cells: Vec::new(),
            size: (0, 0),
        };
        if let Ok(text) = fs::read_to_string(&w.choice) {
            let path = PathBuf::from(text.trim());
            if !text.trim().is_empty() {
                if let Ok(img) = decode(&path) {
                    w.path = Some(path);
                    w.source = Some(img);
                }
            }
        }
        w
    }

    /// Where the choice lives on this machine.
    pub fn choice_file() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("termland")
            .join(CHOICE)
    }

    pub fn is_set(&self) -> bool {
        self.source.is_some()
    }

    /// Use this file as the wallpaper, and remember it.
    ///
    /// The picture is decoded *before* the choice is written: a file that could
    /// not be a wallpaper must not become the one the session loads next time,
    /// or a typo at a prompt costs a boot with no background.
    pub fn set(&mut self, path: &Path) -> Result<(), String> {
        let img = decode(path).map_err(|e| format!("can't use {} as a wallpaper: {e}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.source = Some(img);
        // Force a fresh sampling: the new picture has nothing to do with the
        // old geometry.
        self.size = (0, 0);
        self.cells.clear();
        if let Some(dir) = self.choice.parent() {
            let _ = fs::create_dir_all(dir);
        }
        fs::write(&self.choice, format!("{}\n", path.display()))
            .map_err(|e| format!("couldn't remember the wallpaper: {e}"))
    }

    /// Go back to the gradient.
    pub fn clear(&mut self) -> Result<(), String> {
        self.path = None;
        self.source = None;
        self.cells.clear();
        self.size = (0, 0);
        match fs::remove_file(&self.choice) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("couldn't forget the wallpaper: {e}")),
        }
    }

    /// The picture sampled to `area`, one pair per cell. Recomputed only when
    /// the area changes — a wallpaper is drawn on every frame of the desktop,
    /// and resampling a photograph per frame would be visible as a stutter.
    pub fn cells_for(&mut self, area: Rect) -> &[(Color, Color)] {
        if self.size != (area.width, area.height) {
            self.rebuild(area);
        }
        &self.cells
    }

    /// The colours already sampled for this cell, when the cache matches
    /// `area`. Used for the tile backgrounds, so a highlighted icon sits on the
    /// picture instead of on a rectangle of the gradient.
    pub fn cell(&self, area: Rect, x: u16, y: u16) -> Option<(Color, Color)> {
        if self.size != (area.width, area.height) {
            return None;
        }
        let (cx, cy) = (x.checked_sub(area.x)?, y.checked_sub(area.y)?);
        if cx >= area.width || cy >= area.height {
            return None;
        }
        self.cells
            .get(cy as usize * area.width as usize + cx as usize)
            .copied()
    }

    fn rebuild(&mut self, area: Rect) {
        let (w, h) = (area.width.max(1) as u32, area.height.max(1) as u32);
        self.size = (area.width, area.height);
        self.cells.clear();
        let Some(src) = self.source.as_ref() else {
            return;
        };
        let (tw, th) = (w, h * 2);
        let (x, y, cw, ch) = crop_rect(src.width(), src.height(), tw, th);
        let small = src
            .crop_imm(x, y, cw, ch)
            .resize_exact(tw, th, FilterType::Triangle)
            .to_rgb8();
        self.cells.reserve((tw * th / 2) as usize);
        for row in (0..th).step_by(2) {
            for col in 0..tw {
                self.cells.push((
                    tint(small.get_pixel(col, row)),
                    tint(small.get_pixel(col, row + 1)),
                ));
            }
        }
    }
}

fn tint(p: &Rgb<u8>) -> Color {
    theme::blend(Color::Rgb(p[0], p[1], p[2]), theme::BG, DIM)
}

fn decode(path: &Path) -> Result<DynamicImage, String> {
    image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

/// The biggest centred crop of `(iw, ih)` that has the aspect ratio of
/// `(aw, ah)` — "fill the screen, cut the overflow off both sides" instead of
/// squashing the picture to fit it.
///
/// Integer arithmetic on purpose: cross-multiplying compares the two ratios
/// exactly, where a float ratio can be equal-but-not-quite for one pixel and
/// produce a one-column strip of stretched image.
///
/// A picture with fewer pixels than the target has in one direction cannot be
/// cropped up to the target's shape — there is nothing to discard — so the
/// whole source comes back and the resize stretches it. That only matters for
/// pictures smaller than the screen, where a stretch is the only option that
/// shows the whole thing.
pub fn crop_rect(iw: u32, ih: u32, aw: u32, ah: u32) -> (u32, u32, u32, u32) {
    let (iw, ih) = (iw.max(1), ih.max(1));
    let (aw, ah) = (aw.max(1), ah.max(1));
    let (iw64, ih64) = (u64::from(iw), u64::from(ih));
    let (aw64, ah64) = (u64::from(aw), u64::from(ah));
    // Rounded, not truncated: half a pixel of aspect error is a stretch nobody
    // sees, and a truncated one is a whole column of it.
    let (cw, ch) = if iw64 * ah64 >= aw64 * ih64 {
        // At least as wide as the screen: keep the height, trim the sides.
        let cw = ((ih64 * aw64 + ah64 / 2) / ah64) as u32;
        if cw >= 1 {
            (cw, ih)
        } else {
            (iw, ih)
        }
    } else {
        let ch = ((iw64 * ah64 + aw64 / 2) / aw64) as u32;
        if ch >= 1 {
            (iw, ch)
        } else {
            (iw, ih)
        }
    };
    ((iw - cw) / 2, (ih - ch) / 2, cw, ch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbImage;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "termland-wallpaper-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).expect("temp dir");
        p
    }

    /// A real PNG, written by the encoder, so decode() is exercised for real
    /// rather than against a stub.
    fn png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let img = RgbImage::from_fn(w, h, |x, y| {
            Rgb([(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 128])
        });
        let path = dir.join(name);
        img.save(&path).expect("write png");
        path
    }

    #[test]
    fn the_crop_fills_the_screen_without_stretching_it() {
        // A square picture on a 2:1 area loses half its height, centred.
        assert_eq!(crop_rect(1000, 1000, 200, 100), (0, 250, 1000, 500));
        // A wide picture on a square area loses its sides, centred.
        assert_eq!(crop_rect(2000, 1000, 100, 100), (500, 0, 1000, 1000));
        // An exact match is untouched, and stays at the origin.
        assert_eq!(crop_rect(800, 600, 800, 600), (0, 0, 800, 600));
    }

    #[test]
    fn no_crop_ever_leaves_the_picture_or_loses_its_shape() {
        // The one invariant a "fill the screen" crop has: it is inside the
        // source, and its aspect ratio is the target's. Checked by
        // cross-multiplying, which is exact.
        for (iw, ih) in [(1, 1), (7, 3), (3, 7), (1920, 1080), (640, 480), (13, 97)] {
            for (aw, ah) in [(1, 1), (2, 1), (80, 24), (100, 1), (1, 100), (41, 67)] {
                let (x, y, cw, ch) = crop_rect(iw, ih, aw, ah);
                assert!(x + cw <= iw, "crop {iw}x{ih} -> {aw}x{ah} runs right");
                assert!(y + ch <= ih, "crop {iw}x{ih} -> {aw}x{ah} runs down");
                assert!(cw >= 1 && ch >= 1, "empty crop");
                if (cw, ch) == (iw, ih) {
                    // Nothing was discarded: either the shapes already match,
                    // or the source is too small to be cropped up to them.
                    continue;
                }
                let left = u64::from(cw) * u64::from(ah);
                let right = u64::from(aw) * u64::from(ch);
                // Rounding to a whole pixel is half a pixel of aspect error,
                // which is what the tolerance is: half the target's other side.
                assert!(
                    left.abs_diff(right) <= u64::from(aw.max(ah)) / 2 + 1,
                    "{iw}x{ih} -> {aw}x{ah}: {cw}x{ch} is not {aw}:{ah}"
                );
            }
        }
    }

    #[test]
    fn a_chosen_wallpaper_survives_a_restart() {
        let dir = tmpdir("roundtrip");
        let pic = png(&dir, "sky.png", 16, 9);
        let choice = dir.join("wallpaper.path");

        let mut w = Wallpaper::from_choice(choice.clone());
        assert!(!w.is_set(), "nothing chosen yet");
        w.set(&pic).expect("set");
        assert_eq!(w.path.as_deref(), Some(pic.as_path()));

        // What the next boot reads.
        let again = Wallpaper::from_choice(choice);
        assert!(again.is_set());
        assert_eq!(again.path.as_deref(), Some(pic.as_path()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_picture_is_sampled_to_one_pair_of_colours_per_cell() {
        let dir = tmpdir("cells");
        let pic = png(&dir, "grad.png", 40, 40);
        let mut w = Wallpaper::from_choice(dir.join("wallpaper.path"));
        w.set(&pic).expect("set");
        let area = Rect::new(0, 0, 20, 10);
        let cells = w.cells_for(area);
        assert_eq!(cells.len(), 200, "one pair per cell");
        assert!(cells.iter().any(|(top, bottom)| top != bottom), "the picture is not flat");

        // A second call at the same size is the cached one, not a resample.
        assert_eq!(w.cells_for(area).len(), 200);
        // A new size resamples: the desktop changed shape.
        assert_eq!(w.cells_for(Rect::new(0, 0, 9, 4)).len(), 36);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_a_wallpaper_is_refused_and_changes_nothing() {
        let dir = tmpdir("refuse");
        let good = png(&dir, "good.png", 8, 8);
        let bad = dir.join("notes.txt");
        fs::write(&bad, b"this is not a png").expect("write");

        let mut w = Wallpaper::from_choice(dir.join("wallpaper.path"));
        w.set(&good).expect("the good one is accepted");
        let err = w.set(&bad).expect_err("text is not a picture");
        assert!(err.contains("notes.txt"), "{err}");
        assert_eq!(
            w.path.as_deref(),
            Some(good.as_path()),
            "a failed choice must not replace the working one"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_picture_starts_the_session_on_the_gradient() {
        let dir = tmpdir("gone");
        let pic = png(&dir, "there.png", 8, 8);
        let choice = dir.join("wallpaper.path");
        let mut w = Wallpaper::from_choice(choice.clone());
        w.set(&pic).expect("set");
        fs::remove_file(&pic).expect("delete the picture");

        let again = Wallpaper::from_choice(choice);
        assert!(!again.is_set(), "no picture, no wallpaper, and no panic");

        // Forgetting it is the same state, and is idempotent.
        let mut w = Wallpaper::from_choice(dir.join("wallpaper.path"));
        assert!(w.clear().is_ok());
        assert!(!w.is_set());
        assert!(w.clear().is_ok(), "clearing twice is not an error");
        let _ = fs::remove_dir_all(&dir);
    }
}
