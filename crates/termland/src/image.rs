//! Image panes.
//!
//! The pixels never touch ratatui's cell grid directly: `ratatui-image` picks
//! the best protocol the host terminal advertises (kitty graphics with unicode
//! placeholders, sixel, iterm2, or halfblocks as the floor) and renders
//! placeholder cells that ratatui can diff like any other text.

use anyhow::{Context, Result};
use image::{DynamicImage, Rgb, RgbImage};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::path::{Path, PathBuf};

pub struct ImagePane {
    pub path: Option<PathBuf>,
    pub label: String,
    pub proto: StatefulProtocol,
    pub dims: (u32, u32),
    pub protocol: String,
}

impl ImagePane {
    pub fn from_path(picker: &Picker, path: &Path) -> Result<Self> {
        let img = image::ImageReader::open(path)
            .with_context(|| format!("open {}", path.display()))?
            .with_guessed_format()
            .context("sniff image format")?
            .decode()
            .with_context(|| format!("decode {}", path.display()))?;
        Ok(Self::from_dynamic(picker, img, Some(path.to_path_buf())))
    }

    pub fn from_dynamic(picker: &Picker, img: DynamicImage, path: Option<PathBuf>) -> Self {
        let dims = (img.width(), img.height());
        let label = path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "image".to_string());
        let proto = picker.new_resize_protocol(img);
        Self {
            path,
            label,
            proto,
            dims,
            protocol: protocol_name(picker),
        }
    }

    /// The built-in demo picture: a gradient "blueprint" plate with a wordmark
    /// drawn from a 5x7 bitmap font, so a fresh install has something real to
    /// show that did not come off the disk.
    pub fn demo(picker: &Picker) -> Self {
        Self::from_dynamic(picker, DynamicImage::ImageRgb8(demo_image()), None)
    }
}

fn protocol_name(picker: &Picker) -> String {
    format!("{:?}", picker.protocol_type()).to_lowercase()
}

const BG_A: Rgb<u8> = Rgb([0x1a, 0x22, 0x26]);
const BG_B: Rgb<u8> = Rgb([0x2c, 0x44, 0x4a]);
const ACCENT: Rgb<u8> = Rgb([0x9b, 0xbf, 0xb4]);
const GRID: Rgb<u8> = Rgb([0x2f, 0x42, 0x47]);

fn lerp(a: Rgb<u8>, b: Rgb<u8>, t: f32) -> Rgb<u8> {
    let t = t.clamp(0.0, 1.0);
    Rgb([
        (a.0[0] as f32 + (b.0[0] as f32 - a.0[0] as f32) * t) as u8,
        (a.0[1] as f32 + (b.0[1] as f32 - a.0[1] as f32) * t) as u8,
        (a.0[2] as f32 + (b.0[2] as f32 - a.0[2] as f32) * t) as u8,
    ])
}

pub fn demo_image() -> RgbImage {
    const W: u32 = 1600;
    const H: u32 = 1000;
    let mut img = RgbImage::new(W, H);
    let (cw, ch) = (W as f32, H as f32);

    for y in 0..H {
        for x in 0..W {
            let t = (x as f32 / cw * 0.65) + (y as f32 / ch * 0.35);
            let mut px = lerp(BG_A, BG_B, t);

            // A soft accent glow in the upper right.
            let dx = (x as f32 - cw * 0.82) / (cw * 0.45);
            let dy = (y as f32 - ch * 0.18) / (ch * 0.55);
            let glow = (1.0 - (dx * dx + dy * dy)).max(0.0);
            px = lerp(px, ACCENT, glow * glow * 0.16);

            // Blueprint grid every 50px.
            if x % 50 == 0 || y % 50 == 0 {
                px = lerp(px, GRID, 0.55);
            }

            img.put_pixel(x, y, px);
        }
    }

    // Corner brackets, the cheapest way to make a rectangle look deliberate.
    let m = 24u32;
    let len = 90u32;
    let thick = 4u32;
    for i in 0..len {
        for t in 0..thick {
            for (x, y) in [
                (m + i, m + t),
                (m + t, m + i),
                (W - 1 - m - i, m + t),
                (W - 1 - m - t, m + i),
                (m + i, H - 1 - m - t),
                (m + t, H - 1 - m - i),
                (W - 1 - m - i, H - 1 - m - t),
                (W - 1 - m - t, H - 1 - m - i),
            ] {
                if x < W && y < H {
                    img.put_pixel(x, y, ACCENT);
                }
            }
        }
    }

    draw_wordmark(&mut img, "TERMLAND", 18);

    // A baseline rule under the wordmark.
    let y0 = (H / 2 + 150).min(H - 4);
    for x in 320..(W - 320) {
        for t in 0..2 {
            if y0 + t < H {
                img.put_pixel(x, y0 + t, lerp(ACCENT, BG_B, 0.45));
            }
        }
    }

    img
}

fn draw_wordmark(img: &mut RgbImage, text: &str, scale: u32) {
    let glyphs: Vec<[&'static str; 7]> = text.chars().filter_map(glyph_rows).collect();
    if glyphs.is_empty() {
        return;
    }
    let adv = 6 * scale;
    let total_w = glyphs.len() as u32 * adv - scale;
    let total_h = 7 * scale;
    let x0 = (img.width().saturating_sub(total_w)) / 2;
    let y0 = (img.height().saturating_sub(total_h)) / 2;

    for (i, rows) in glyphs.iter().enumerate() {
        let gx = x0 + i as u32 * adv;
        for (row, line) in rows.iter().enumerate() {
            for (col, ch) in line.as_bytes().iter().enumerate() {
                if *ch != b'1' {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = gx + col as u32 * scale + dx;
                        let py = y0 + row as u32 * scale + dy;
                        if px < img.width() && py < img.height() {
                            img.put_pixel(px, py, ACCENT);
                        }
                    }
                }
            }
        }
    }
}

/// 5x7 bitmap glyphs — just enough letters to spell the project name.
fn glyph_rows(c: char) -> Option<[&'static str; 7]> {
    Some(match c {
        'T' => [
            "11111", "00100", "00100", "00100", "00100", "00100", "00100",
        ],
        'E' => [
            "11111", "10000", "10000", "11110", "10000", "10000", "11111",
        ],
        'R' => [
            "11110", "10001", "10001", "11110", "10100", "10010", "10001",
        ],
        'M' => [
            "10001", "11011", "10101", "10101", "10001", "10001", "10001",
        ],
        'L' => [
            "10000", "10000", "10000", "10000", "10000", "10000", "11111",
        ],
        'A' => [
            "00100", "01010", "10001", "10001", "11111", "10001", "10001",
        ],
        'N' => [
            "10001", "11001", "11001", "10101", "10011", "10011", "10001",
        ],
        'D' => [
            "11110", "10001", "10001", "10001", "10001", "10001", "11110",
        ],
        ' ' => [
            "00000", "00000", "00000", "00000", "00000", "00000", "00000",
        ],
        _ => return None,
    })
}
