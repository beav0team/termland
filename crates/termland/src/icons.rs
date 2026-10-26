//! File icons, drawn as half blocks.
//!
//! A terminal cannot scale a font, so a 1-cell Nerd Font glyph inside a 12×5
//! tile is a smudge: the tile is 120 × 110 pixels and the icon is 10 × 22 of
//! them. Every "big icon" grid that looks right in a terminal — `nnn`'s
//! previews, `lf`'s sixels — is either an image or a bitmap, and an image needs
//! the kitty protocol while a bitmap needs nothing at all.
//!
//! So each icon is a small mask of half-rows, painted with `▀▄█`: eight cells
//! wide and three rows tall, four times the resolution of a glyph, in two
//! shades, and identical in every terminal that has block characters.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme;

/// What the icon stands for. Matched from the extension, never from reading
/// the file: a file manager that opens every file to decide what it is cannot
/// show a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Parent,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Pdf,
    Text,
    File,
}

impl Kind {
    /// From a file name, the only thing a listing has.
    pub fn of(name: &str, is_dir: bool, is_parent: bool) -> Kind {
        if is_parent {
            return Kind::Parent;
        }
        if is_dir {
            return Kind::Folder;
        }
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "avif" | "tiff" | "heic" => {
                Kind::Image
            }
            "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "mpg" => Kind::Video,
            "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" | "aac" => Kind::Audio,
            "zip" | "gz" | "xz" | "tar" | "bz2" | "zst" | "7z" | "rar" | "deb" | "iso" => {
                Kind::Archive
            }
            "pdf" => Kind::Pdf,
            "rs" | "toml" | "json" | "yaml" | "yml" | "js" | "ts" | "tsx" | "jsx" | "py" | "c"
            | "h" | "cpp" | "go" | "sh" | "lua" | "kt" | "java" | "html" | "css" | "scss" | "md"
            | "sql" => Kind::Code,
            "txt" | "log" | "conf" | "ini" | "csv" | "env" => Kind::Text,
            _ => Kind::File,
        }
    }

    /// The colour of the body. Types that people actually hunt for get a hue;
    /// everything else stays quiet, or the grid turns into a fruit salad.
    pub fn colour(self) -> Color {
        match self {
            Kind::Folder => theme::ACCENT,
            Kind::Parent => theme::FG_DIM,
            Kind::Image => Color::Rgb(0xa8, 0xbe, 0x8c),
            Kind::Video => Color::Rgb(0x9d, 0x9b, 0xd0),
            Kind::Audio => Color::Rgb(0xc2, 0x9a, 0xc8),
            Kind::Archive => theme::WARN,
            Kind::Code => Color::Rgb(0x8f, 0xa9, 0xcf),
            Kind::Pdf => theme::ERR,
            Kind::Text | Kind::File => Color::Rgb(0x9a, 0xa8, 0xad),
        }
    }

    /// The glyph stamped into the middle of the page — a file is a page, and
    /// what kind of page it is belongs on it. Folder and parent have no stamp:
    /// their silhouettes already say it.
    pub fn stamp(self) -> Option<&'static str> {
        Some(match self {
            Kind::Folder => return None,
            // The parent of a folder is a folder with an arrow in it: an arrow
            // on its own is a cross at this size, and "up" is what the icon has
            // to say.
            Kind::Parent => "\u{f062}",
            Kind::Image => "\u{f1c5}",
            Kind::Video => "\u{f1c8}",
            Kind::Audio => "\u{f1c7}",
            Kind::Archive => "\u{f1c6}",
            Kind::Code => "\u{f1c9}",
            Kind::Pdf => "\u{f1c1}",
            Kind::Text => "\u{f0f6}",
            Kind::File => "\u{f016}",
        })
    }

    /// Rows of half-cells, top to bottom. `#` is the body, `+` the lit edge,
    /// `-` the shadow — three shades are what turns a rectangle into a shape.
    ///
    /// Ten wide instead of eight because a folder needs a *tab*: at eight the
    /// tab is a bump on a rectangle and the icon reads as a wedding cake.
    fn mask(self) -> &'static [&'static str] {
        match self {
            // A tab on the left and a body under it. The tab is a whole cell
            // row, not a row of half-cells: at ten pixels wide the silhouette
            // is the only thing that survives, and a folder is a tab.
            Kind::Folder | Kind::Parent => &[
                ".####.....",
                ".####.....",
                "++++++++++",
                "##########",
                "##########",
                "##########",
                "##########",
                "##########",
                "##########",
                "----------",
            ],
            // A sheet of paper, with its top-right corner turned down.
            _ => &[
                ".+++++++..",
                ".+#####+..",
                ".+######+.",
                ".+######+.",
                ".+######+.",
                ".+######+.",
                ".+######+.",
                ".+######+.",
                ".+######+.",
                ".++++++++.",
            ],
        }
    }
}

/// The two halves of a cell are two colours, not one. A half-block glyph is
/// drawn with the foreground in one half and the background in the other, so a
/// mask cell can be "light on top, dark underneath" — which is the difference
/// between an edge that is one pixel thick and an edge that swallows a whole
/// row of the icon.
fn halves(top: Option<Color>, bottom: Option<Color>, under: Color) -> Option<(char, Color, Color)> {
    match (top, bottom) {
        (None, None) => None,
        (Some(t), None) => Some(('▀', t, under)),
        (None, Some(b)) => Some(('▄', b, under)),
        (Some(t), Some(b)) if t == b => Some(('█', t, t)),
        (Some(t), Some(b)) => Some(('▀', t, b)),
    }
}

/// Paint one icon into `rect`, in the style of the tile behind it.
///
/// The icon is centred and never scaled: a 10-wide mask on a 12-wide tile keeps
/// its proportions, and a narrow tile shows the middle of the icon rather than
/// a squashed one.
pub fn paint(buf: &mut Buffer, rect: Rect, kind: Kind, selected: bool, bg: Color) {
    let mask = kind.mask();
    let rows = mask.len() / 2;
    let cols = mask[0].chars().count();
    if rect.width == 0 || rect.height == 0 || rows == 0 {
        return;
    }

    // A folder is a solid shape; a sheet of paper is a card with the type's
    // colour in its border and its stamp. Painting the page in the full type
    // colour turns the whole grid into pale rectangles of one weight, and the
    // colour stops being information.
    let tone = kind.colour();
    let mut body = match kind {
        Kind::Folder | Kind::Parent => tone,
        _ => theme::blend(tone, bg, 0.42),
    };
    if selected {
        body = theme::blend(body, Color::Rgb(0xff, 0xff, 0xff), 0.22);
    }
    let edge = theme::blend(tone, Color::Rgb(0xff, 0xff, 0xff), 0.3);
    let shadow = theme::blend(body, theme::BG, 0.5);

    let colour_of = |c: char| match c {
        '#' => Some(body),
        '+' => Some(edge),
        '-' => Some(shadow),
        _ => None,
    };

    let x0 = rect.x + rect.width.saturating_sub(cols as u16) / 2;
    let y0 = rect.y + rect.height.saturating_sub(rows as u16) / 2;
    for (r, pair) in mask.chunks(2).enumerate() {
        let y = y0 + r as u16;
        if y >= rect.y + rect.height {
            break;
        }
        let top: Vec<char> = pair[0].chars().collect();
        let bottom: Vec<char> = pair.get(1).map(|l| l.chars().collect()).unwrap_or_default();
        for c in 0..cols {
            let x = x0 + c as u16;
            if x >= rect.x + rect.width {
                break;
            }
            let t = top.get(c).copied().and_then(colour_of);
            let b = bottom.get(c).copied().and_then(colour_of);
            let Some((sym, fg, cell_bg)) = halves(t, b, bg) else {
                continue;
            };
            buf[(x, y)].set_symbol(&sym.to_string()).set_style(
                Style::default()
                    .fg(fg)
                    .bg(cell_bg)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            );
        }
    }

    // The stamp goes on the middle row of the page, cut out of the body.
    if let Some(stamp) = kind.stamp() {
        let sx = x0 + cols as u16 / 2;
        let sy = y0 + rows as u16 / 2;
        if sx < rect.x + rect.width && sy < rect.y + rect.height && sy >= rect.y {
            buf.set_stringn(
                sx,
                sy,
                stamp,
                1,
                Style::default()
                    .fg(theme::blend(tone, Color::Rgb(0xff, 0xff, 0xff), 0.3))
                    .bg(body)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(kind: Kind) -> (Vec<String>, Buffer) {
        let area = Rect::new(0, 0, 12, 3);
        let mut b = Buffer::empty(area);
        paint(&mut b, area, kind, false, theme::BG);
        let mut rows = Vec::new();
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                let c = &b[(x, y)];
                line.push_str(if c.symbol() == " " { "." } else { "#" });
            }
            rows.push(line);
        }
        (rows, b)
    }

    #[test]
    fn a_file_name_picks_the_icon_and_nothing_else_does() {
        assert_eq!(Kind::of("holiday.JPG", false, false), Kind::Image);
        assert_eq!(Kind::of("film.mkv", false, false), Kind::Video);
        assert_eq!(Kind::of("song.flac", false, false), Kind::Audio);
        assert_eq!(Kind::of("backup.tar.gz", false, false), Kind::Archive);
        assert_eq!(Kind::of("main.rs", false, false), Kind::Code);
        assert_eq!(Kind::of("invoice.pdf", false, false), Kind::Pdf);
        assert_eq!(Kind::of("notes.txt", false, false), Kind::Text);
        assert_eq!(Kind::of("LICENSE", false, false), Kind::File);
        assert_eq!(Kind::of("Pictures", true, false), Kind::Folder);
        assert_eq!(Kind::of("..", true, true), Kind::Parent);
        // A directory called "x.png" is a directory.
        assert_eq!(Kind::of("x.png", true, false), Kind::Folder);
    }

    #[test]
    fn every_icon_paints_inside_its_tile() {
        for kind in [
            Kind::Folder,
            Kind::Parent,
            Kind::Image,
            Kind::Video,
            Kind::Audio,
            Kind::Archive,
            Kind::Code,
            Kind::Pdf,
            Kind::Text,
            Kind::File,
        ] {
            let (rows, b) = drawn(kind);
            let lit = rows.iter().map(|r| r.matches('#').count()).sum::<usize>();
            assert!(lit >= 8, "{kind:?} painted {lit} cells");
            // Nothing is ever painted outside the rows it was given.
            assert_eq!(b.content.len(), 36);
        }
    }

    #[test]
    fn a_folder_and_a_page_are_different_shapes() {
        let folder = drawn(Kind::Folder).0;
        let page = drawn(Kind::File).0;
        assert_ne!(folder, page);
        // The folder's tab is in the top row; the page has none.
        assert!(folder[0].matches('#').count() < page[0].matches('#').count());
    }

    #[test]
    fn a_narrow_tile_shows_the_middle_of_the_icon_rather_than_a_squashed_one() {
        let area = Rect::new(0, 0, 4, 3);
        let mut b = Buffer::empty(area);
        paint(&mut b, area, Kind::Image, false, theme::BG);
        let lit = b.content.iter().filter(|c| c.symbol() != " ").count();
        assert!(lit > 0, "a 4-wide tile painted nothing");
    }
}
