//! Ant-Dark-ish palette, borrowed from the Hyprland config on this machine.

use ratatui::style::Color;

pub const BG: Color = Color::Rgb(0x1e, 0x28, 0x2c);
pub const BG_ALT: Color = Color::Rgb(0x25, 0x32, 0x37);
pub const FG: Color = Color::Rgb(0xd8, 0xde, 0xe9);
pub const FG_DIM: Color = Color::Rgb(0x77, 0x89, 0x90);
pub const ACCENT: Color = Color::Rgb(0x9b, 0xbf, 0xb4);
pub const ACCENT_FG: Color = Color::Rgb(0x14, 0x1c, 0x1f);
pub const BORDER: Color = Color::Rgb(0x33, 0x43, 0x48);
pub const WARN: Color = Color::Rgb(0xd0, 0x87, 0x70);
pub const ERR: Color = Color::Rgb(0xbf, 0x61, 0x6a);
pub const OK: Color = Color::Rgb(0xa3, 0xbe, 0x8c);

/// The desktop's heat ramp, cold → accent → warm → hot, in the shape
/// `tui-chart` wants (`Gradient::Custom`). The crate interpolates between the
/// stops, so there is no interpolation code here: one palette, one authority.
pub fn thermal() -> Vec<(u8, u8, u8)> {
    [
        Color::Rgb(0x2b, 0x3a, 0x40),
        Color::Rgb(0x4f, 0x7f, 0x8f),
        ACCENT,
        WARN,
        Color::Rgb(0xe0, 0x62, 0x5a),
    ]
    .into_iter()
    .map(rgb)
    .collect()
}

/// The RGB of a palette colour. Non-RGB terminal colours (the 16 named ones)
/// have no fixed value, so they fall back to the foreground.
pub fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0xd8, 0xde, 0xe9),
    }
}

/// A colour between two others. `t` 0.0 is `a`, 1.0 is `b`. Used for hover
/// highlights that fade in, which cannot be done with a fixed palette: the
/// highlight has to be a *percentage* of the way to the accent.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return if t < 0.5 { a } else { b };
    };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_hits_both_ends_and_the_middle() {
        assert_eq!(blend(BG, ACCENT, 0.0), BG);
        assert_eq!(blend(BG, ACCENT, 1.0), ACCENT);
        let mid = blend(BG, ACCENT, 0.5);
        assert_ne!(mid, BG);
        assert_ne!(mid, ACCENT);
    }

    #[test]
    fn blend_clamps_outside_zero_to_one() {
        assert_eq!(blend(BG, ACCENT, -3.0), BG);
        assert_eq!(blend(BG, ACCENT, 4.0), ACCENT);
    }
}
