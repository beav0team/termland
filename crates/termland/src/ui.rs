//! All drawing. Nothing here mutates app state except through `Frame`.

use crate::surface::Frame;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui_image::{Resize, StatefulImage};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Pane};
use crate::desktop;
use crate::files::{self, FilePane, FileView};
use crate::icons;
use crate::image::ImagePane;
use crate::pty::TermPane;
use crate::theme;
use crate::wallpaper;

pub fn draw_background(f: &mut Frame, area: Rect) {
    f.render(
        Block::default().style(Style::default().bg(theme::BG).fg(theme::FG)),
        area,
    );
}

/// Draw a pane's title bar and return the close box's hit rect.
///
/// The close box sits on the left, where a window's controls are on every
/// desktop worth using. There is no `WxH` readout any more: nobody picks a
/// window by its column count, and the space belongs to the title.
pub fn draw_header(
    f: &mut Frame,
    area: Rect,
    pane: &Pane,
    focused: bool,
    close_hover: f32,
) -> Rect {
    let close = Rect {
        x: area.x,
        y: area.y,
        width: area.width.min(3),
        height: area.height.min(1),
    };
    if area.width == 0 || area.height == 0 {
        return close;
    }
    let bg = if focused {
        theme::ACCENT
    } else {
        theme::BG_ALT
    };
    let buf = f.buffer_mut();
    for x in area.x..area.x + area.width {
        buf[(x, area.y)]
            .set_symbol(" ")
            .set_style(Style::default().bg(bg));
    }

    // The close box lights up as the pointer reaches it. `close_hover` is
    // 0.0 → 1.0 so the highlight fades rather than switching.
    let close_bg = theme::blend(bg, theme::ERR, close_hover * 0.85);
    let close_style = if focused || close_hover > 0.0 {
        Style::default()
            .fg(theme::blend(
                if focused { theme::WARN } else { theme::FG_DIM },
                theme::BG,
                close_hover,
            ))
            .bg(close_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::FG_DIM).bg(bg)
    };
    buf.set_stringn(area.x + 1, area.y, "×", 1, close_style);
    if area.width <= 3 {
        return close;
    }

    let icon = match pane.kind() {
        "image" => "▣",
        "web" => "◈",
        "files" => "▤",
        _ => "▸",
    };
    let mut left = format!("{icon} {} ", pane.title());
    if focused {
        left.push_str(&format!("· {} ", pane.kind()));
    }
    let avail = area.width.saturating_sub(3) as usize;
    let trimmed: String = left.chars().take(avail).collect();
    let style = if focused {
        Style::default()
            .fg(theme::ACCENT_FG)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::FG_DIM).bg(bg)
    };
    buf.set_stringn(area.x + 3, area.y, &trimmed, avail, style);
    close
}

pub fn draw_term(f: &mut Frame, area: Rect, pane: &TermPane, focused: bool) -> Option<(u16, u16)> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let screen = pane.screen();
    let (rows, cols) = pane.size;
    let buf = f.buffer_mut();
    let scroll = pane.scroll_offset.min(rows.saturating_sub(1));
    let h = area.height.min(rows.saturating_sub(scroll));
    let w = area.width.min(cols);

    for y in 0..h {
        let screen_y = rows.saturating_sub(h) + y + scroll;
        for x in 0..w {
            let Some(cell) = screen.cell(screen_y, x) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let px = area.x + x;
            let py = area.y + y;
            let mut style = Style::default()
                .fg(conv(cell.fgcolor()))
                .bg(conv_bg(cell.bgcolor()));
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.dim() {
                style = style.add_modifier(Modifier::DIM);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if cell.inverse() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let raw = cell.contents();
            let sym = if raw.is_empty() { " " } else { raw };
            buf[(px, py)].set_symbol(sym).set_style(style);
            // Wide glyphs: the continuation column must be skipped by the
            // renderer, or the second half gets overwritten by a blank.
            if sym.width() == 2 && x + 1 < w {
                buf[(px + 1, py)].reset();
                buf[(px + 1, py)].set_diff_option(CellDiffOption::Skip);
            }
        }
    }

    if !focused || screen.hide_cursor() {
        draw_scrollbar(f, area, pane, h, w);
        return None;
    }
    let (cy, cx) = screen.cursor_position();
    let cy = cy.saturating_sub(pane.scroll_offset);
    draw_scrollbar(f, area, pane, h, w);
    if cx < w && cy < h {
        Some((area.x + cx, area.y + cy))
    } else {
        None
    }
}

/// Draw a scrollbar on the right edge of a pane that has more content than
/// fits in the viewport.
fn draw_scrollbar(f: &mut Frame, area: Rect, pane: &TermPane, h: u16, w: u16) {
    let screen = pane.screen();
    let (rows, cols) = pane.size;
    if area.width <= 2 || rows <= h {
        return;
    }
    let buf = f.buffer_mut();
    let total = rows as u32;
    let visible = h as u32;
    let scroll = pane.scroll_offset as u32;
    let max_scroll = total.saturating_sub(visible);
    let thumb_size = (visible * visible / total.max(1)).max(1);
    let thumb_pos = scroll
        .saturating_mul(visible.saturating_sub(thumb_size))
        .checked_div(max_scroll)
        .unwrap_or(0);
    let bar_x = area.x + area.width - 1;
    for y in 0..h {
        let py = area.y + y;
        let in_thumb = y >= thumb_pos as u16 && y < thumb_pos as u16 + thumb_size as u16;
        let ch = if in_thumb { "█" } else { "│" };
        let style = if in_thumb {
            Style::default().fg(theme::ACCENT_FG).bg(theme::ACCENT)
        } else {
            Style::default().fg(theme::BORDER).bg(theme::BG_ALT)
        };
        buf[(bar_x, py)].set_symbol(ch).set_style(style);
    }
    let _ = screen;
    let _ = cols;
    let _ = w;
}

/// One cell of the wallpaper: a vertical ramp from a lifted slate at the top to
/// something darker than the pane background at the bottom, with a soft glow
/// off-centre and a whisper of noise.
///
/// The noise is not decoration. A gradient across fifty rows of a terminal is
/// ten levels of colour, and ten levels band: the noise scatters each boundary
/// so the eye reads a surface instead of a staircase.
pub fn wallpaper_colour(area: Rect, x: u16, y: u16) -> Color {
    let w = area.width.max(1) as f32;
    let h = area.height.max(1) as f32;
    let rx = (x.saturating_sub(area.x)) as f32 / w;
    let ry = (y.saturating_sub(area.y)) as f32 / h;
    let deep = theme::blend(theme::BG, Color::Rgb(0x08, 0x0c, 0x0e), 0.55);
    let lit = theme::blend(theme::BG_ALT, theme::ACCENT, 0.10);
    let mut c = theme::blend(lit, deep, ry.powf(0.85));
    // A cell is about twice as tall as it is wide, so the glow has to be
    // squashed vertically to look round.
    let (gx, gy) = (0.26f32, 0.12f32);
    let dx = (rx - gx) * 1.0;
    let dy = (ry - gy) * 2.0;
    let glow = (1.0 - (dx * dx + dy * dy).sqrt() / 0.62).clamp(0.0, 1.0);
    c = theme::blend(c, theme::ACCENT, 0.13 * glow * glow);
    let n = ((x as u32)
        .wrapping_mul(0x9e37_79b1)
        .wrapping_add((y as u32).wrapping_mul(0x85eb_ca77)))
        >> 11;
    // Both axes go into the hash and the amplitude stays under one colour
    // level: enough to break a band, not enough to look like a texture.
    let jitter = ((n & 7) as f32 - 3.5) * 0.5;
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f32 + jitter).clamp(0.0, 255.0) as u8,
            (g as f32 + jitter).clamp(0.0, 255.0) as u8,
            (b as f32 + jitter).clamp(0.0, 255.0) as u8,
        ),
        other => other,
    }
}

/// The wallpaper: the chosen picture if there is one, the painted gradient
/// otherwise.
///
/// A picture is drawn as half blocks — `▀` carries the top pixel row in the
/// foreground and the bottom one in the background — so a cell holds two pixels
/// and the vertical resolution doubles. On a 100×46 pane that is the difference
/// between a 46-pixel-tall photograph and a 92-pixel one, and the second reads
/// as a picture.
pub fn draw_wallpaper(f: &mut Frame, area: Rect, wp: &mut wallpaper::Wallpaper) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if wp.is_set() {
        let cells = wp.cells_for(area);
        if cells.len() == area.width as usize * area.height as usize {
            let w = area.width as usize;
            let buf = f.buffer_mut();
            for (i, (top, bottom)) in cells.iter().enumerate() {
                let cell = &mut buf[(area.x + (i % w) as u16, area.y + (i / w) as u16)];
                if top == bottom {
                    cell.set_symbol(" ").set_bg(*top);
                } else {
                    cell.set_symbol("▀").set_fg(*top).set_bg(*bottom);
                }
            }
            return;
        }
    }
    let buf = f.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)]
                .set_symbol(" ")
                .set_bg(wallpaper_colour(area, x, y));
        }
    }
}

/// The desktop: wallpaper, then the icons of `~/Desktop` in the same grid the
/// file manager uses. It is drawn only when there is nothing tiled over it, so
/// the wallpaper is never a background for a pane — it *is* the workspace.
pub fn draw_desktop(
    f: &mut Frame,
    area: Rect,
    d: &desktop::Desktop,
    hover_entry: Option<usize>,
    hover: f32,
    wp: &mut wallpaper::Wallpaper,
) {
    draw_wallpaper(f, area, wp);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = f.buffer_mut();
    for pos in 0..d.capacity {
        let idx = d.offset + pos;
        let Some(entry) = d.entries.get(idx) else {
            break;
        };
        let rect = d.tiles.rect(area, pos);
        if rect.width < 4 || rect.height < 2 {
            continue;
        }
        let fade = if hover_entry == Some(idx) { hover } else { 0.0 };
        // A highlighted tile is painted over its own piece of the wallpaper,
        // not over a flat colour: otherwise a selection shows up as a hole in
        // the picture.
        let under = match wp.cell(area, rect.x, rect.y) {
            Some((_, bottom)) => bottom,
            None => wallpaper_colour(area, rect.x, rect.y),
        };
        draw_tile(buf, rect, entry, idx == d.selected, fade, under);
    }
    if d.entries.is_empty() && area.height > 3 {
        buf.set_stringn(
            area.x + 2,
            area.y + 1,
            format!("{} is empty", d.dir.display()),
            area.width.saturating_sub(2) as usize,
            Style::default().fg(theme::FG_DIM),
        );
    }
}

/// The desktop's right-click menu.
pub fn draw_desktop_menu(f: &mut Frame, d: &desktop::Desktop, screen: Rect) {
    let Some(menu) = &d.menu else {
        return;
    };
    let Some(rect) = d.menu_rect(screen) else {
        return;
    };
    let labels: Vec<&str> = d.menu_items().iter().map(|i| i.label()).collect();
    draw_popup(f, rect, &labels, menu.item);
}

/// A pop-up list: a rounded border, one row per label, one row highlighted.
/// The file manager's menu and the desktop's are the same box, drawn twice.
fn draw_popup(f: &mut Frame, rect: Rect, labels: &[&str], selected: usize) {
    f.render(Clear, rect);
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::ACCENT))
        .style(Style::default().bg(theme::BG));
    let inner = block.inner(rect);
    f.render(block, rect);
    let buf = f.buffer_mut();
    for (i, label) in labels.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let style = if i == selected {
            Style::default()
                .fg(theme::ACCENT_FG)
                .bg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG).bg(theme::BG)
        };
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_symbol(" ").set_style(style);
        }
        buf.set_stringn(inner.x, y, format!(" {label}"), inner.width as usize, style);
    }
}

/// The file pane body: rows in list view, tiles in the icon view.
pub fn draw_files(
    f: &mut Frame,
    area: Rect,
    fp: &FilePane,
    _focused: bool,
    hover_row: Option<usize>,
    hover: f32,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if fp.view == FileView::Grid {
        draw_files_grid(f, area, fp, hover_row, hover);
        return;
    }
    let buf = f.buffer_mut();
    let width = area.width as usize;

    for row in 0..area.height as usize {
        let y = area.y + row as u16;
        let idx = fp.offset + row;
        let Some(entry) = fp.entries.get(idx) else {
            for x in 0..area.width {
                buf[(area.x + x, y)].set_style(Style::default().bg(theme::BG));
            }
            continue;
        };
        let selected = idx == fp.selected;
        // Hover is a fade toward the accent, weaker than selection, so a
        // selected row keeps its own emphasis while the pointer passes over.
        let hovered = hover_row == Some(idx);
        let bg = if selected {
            theme::ACCENT
        } else if hovered && hover > 0.0 {
            theme::blend(theme::BG, theme::ACCENT, 0.18 * hover)
        } else {
            theme::BG
        };
        let fg = if selected {
            theme::ACCENT_FG
        } else if entry.is_dir {
            theme::blend(theme::ACCENT, theme::ACCENT_FG, 0.0)
        } else {
            theme::FG
        };
        for x in 0..area.width {
            buf[(area.x + x, y)]
                .set_symbol(" ")
                .set_style(Style::default().bg(bg));
        }

        let size = if entry.is_dir {
            String::new()
        } else {
            files::human_size(entry.size)
        };
        let right = if size.is_empty() {
            String::new()
        } else {
            format!("{size} ")
        };
        let rw = right.chars().count();
        let name_avail = width.saturating_sub(2 + rw);
        let icon = if entry.is_dir { "▸" } else { "·" };
        let label: String = format!("{icon} {}", entry.name)
            .chars()
            .take(name_avail)
            .collect();
        buf.set_stringn(
            area.x + 1,
            y,
            &label,
            name_avail,
            Style::default().fg(fg).bg(bg),
        );
        if rw > 0 && width > rw + 2 {
            let rx = area.x + area.width - rw as u16;
            let style = Style::default()
                .fg(if selected {
                    theme::ACCENT_FG
                } else {
                    theme::FG_DIM
                })
                .bg(bg);
            buf.set_stringn(rx, y, &right, rw, style);
        }
    }
}

/// Split a name into at most `lines` rows of `width` cells. The last row gets
/// an ellipsis rather than a cut word: a file manager that silently hides the
/// end of a name is how people delete the wrong file.
fn wrap_name(name: &str, width: usize, lines: usize) -> Vec<String> {
    if width == 0 || lines == 0 {
        return Vec::new();
    }
    let chars: Vec<char> = name.chars().collect();
    // A name that needs more than one row is balanced across them rather than
    // filled greedily: `HYPRLAND-TRY.txt` in two 14-wide rows should read
    // "HYPRLAND" / "-TRY.txt", not "HYPRLAND-TRY.tx" / "t". A one-character
    // second row is how a file manager makes you guess what you are clicking.
    let per = if chars.len() <= width {
        width
    } else if chars.len() <= width * lines {
        // The break that sits closest to an even split, among the ones that
        // are a break at all: after a space, a dot, an underscore or a dash.
        // `keyboard.jpg` in two ten-wide rows reads "keyboard." / "jpg", and
        // `tyre-prices-2026-09-26.csv` keeps its extension on the second row.
        let balanced = chars.len().div_ceil(lines);
        let mut per = balanced;
        let mut best = usize::MAX;
        for (i, c) in chars.iter().enumerate() {
            if !matches!(c, ' ' | '.' | '_' | '-') {
                continue;
            }
            let left = i + 1;
            if left > width || left < 2 || chars.len() - left > width * (lines - 1) {
                continue;
            }
            let distance = left.abs_diff(balanced);
            if distance < best {
                best = distance;
                per = left;
            }
        }
        per
    } else {
        width
    }
    .max(1);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() && out.len() < lines {
        let last_line = out.len() + 1 == lines;
        let remaining = chars.len() - i;
        let take = if last_line {
            remaining
        } else {
            per.min(remaining)
        };
        if take > width {
            // Room for the ellipsis, and the row before it stays a whole word
            // where one fits.
            let mut text: String = chars[i..i + width - 1].iter().collect();
            if let Some(cut) = text.rfind([' ', '.', '_', '-']) {
                if cut > width / 2 {
                    text.truncate(cut + 1);
                }
            }
            text.push('…');
            out.push(text);
            break;
        }
        out.push(chars[i..i + take].iter().collect());
        i += take;
    }
    out
}

/// One tile: the icon, and the name underneath. Selection fills the tile
/// rather than drawing a box inside it, so the grid still reads as a grid.
fn draw_tile(
    buf: &mut ratatui::buffer::Buffer,
    rect: Rect,
    entry: &files::Entry,
    selected: bool,
    hovered: f32,
    under: Color,
) {
    // A quiet tile paints nothing at all: the caller has already put a
    // background there (a pane's body, or the wallpaper), and repainting it
    // would show up as a rectangle of one flat colour over a gradient.
    let base = if selected {
        theme::blend(under, theme::ACCENT, 0.20)
    } else if hovered > 0.0 {
        theme::blend(under, theme::ACCENT, 0.16 * hovered)
    } else {
        under
    };
    if selected || hovered > 0.0 {
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                let cell = &mut buf[(x, y)];
                cell.set_symbol(" ");
                cell.set_bg(base);
            }
        }
    }

    let name_rows = 2u16.min(rect.height);
    let icon_h = rect.height.saturating_sub(name_rows).max(1);
    let icon = Rect {
        x: rect.x + 1,
        y: rect.y,
        width: rect.width.saturating_sub(2),
        height: icon_h,
    };
    let kind = icons::Kind::of(&entry.name, entry.is_dir, entry.is_parent());
    icons::paint(buf, icon, kind, selected, base);

    let name_w = rect.width.saturating_sub(2) as usize;
    let name_x = rect.x + 1;
    let lines = wrap_name(&entry.name, name_w, name_rows as usize);
    // The name sits on a darkened copy of whatever is under it. Over a flat
    // pane that is the pane; over a photograph it is the photograph pulled far
    // enough toward the background that an icon name never disappears into a
    // bright sky. It costs one cell of padding on each side.
    let pill = theme::blend(base, theme::BG, 0.55);
    for (i, line) in lines.iter().enumerate() {
        let y = rect.y + icon_h + i as u16;
        if y >= rect.y + rect.height {
            break;
        }
        let pad = name_w.saturating_sub(line.chars().count()) / 2;
        let text_x = name_x + pad as u16;
        let end = text_x + line.chars().count() as u16;
        for x in text_x.saturating_sub(1).max(rect.x)..=(end).min(rect.x + rect.width - 1) {
            buf[(x, y)].set_bg(pill);
        }
        buf.set_stringn(
            text_x,
            y,
            line,
            name_w.saturating_sub(pad),
            Style::default().fg(theme::FG).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        );
    }
}

/// The icon view: a grid of tiles, each a chip with the type's glyph and the
/// name under it. Every tile is painted edge to edge with its own background,
/// so hover and selection are the tile, not a box drawn inside it.
fn draw_files_grid(
    f: &mut Frame,
    area: Rect,
    fp: &FilePane,
    hover_entry: Option<usize>,
    hover: f32,
) {
    let buf = f.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_symbol(" ").set_bg(theme::BG);
        }
    }
    let capacity = fp.tiles.capacity(area);
    for pos in 0..capacity {
        let idx = fp.offset + pos;
        let Some(entry) = fp.entries.get(idx) else {
            break;
        };
        let rect = fp.tiles.rect(area, pos);
        if rect.width < 4 || rect.height < 2 {
            continue;
        }
        let fade = if hover_entry == Some(idx) { hover } else { 0.0 };
        draw_tile(buf, rect, entry, idx == fp.selected, fade, theme::BG);
    }
}

/// The context menu and the modal, drawn after every pane so they can spill
/// over the pane that owns them.
pub fn draw_file_overlay(f: &mut Frame, fp: &FilePane, screen: Rect) {
    if let Some(menu) = &fp.menu {
        let items = fp.menu_items();
        let labels: Vec<&str> = items.iter().map(|i| i.label()).collect();
        let rect = files::menu_rect(menu, &items, screen);
        draw_popup(f, rect, &labels, menu.item);
    }

    if let Some(dialog) = &fp.dialog {
        let rect = files::dialog_rect(screen);
        f.render(Clear, rect);
        let block = Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::WARN))
            .title(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    dialog.prompt(),
                    Style::default()
                        .fg(theme::WARN)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .style(Style::default().bg(theme::BG));
        let inner = block.inner(rect);
        f.render(block, rect);
        if inner.height < 2 {
            return;
        }
        let mut lines = vec![Line::from("")];
        if dialog.is_confirm() {
            lines.push(Line::from(vec![
                Span::styled("  y", Style::default().fg(theme::ACCENT)),
                Span::styled(" delete   ·   ", Style::default().fg(theme::FG_DIM)),
                Span::styled("n / Esc", Style::default().fg(theme::ACCENT)),
                Span::styled(" cancel", Style::default().fg(theme::FG_DIM)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("  ▸ ", Style::default().fg(theme::ACCENT)),
                Span::styled(
                    format!("{}█", dialog.input),
                    Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Enter", Style::default().fg(theme::ACCENT)),
                Span::styled(" apply   ·   ", Style::default().fg(theme::FG_DIM)),
                Span::styled("Esc", Style::default().fg(theme::ACCENT)),
                Span::styled(" cancel", Style::default().fg(theme::FG_DIM)),
            ]));
        }
        if !dialog.error.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", dialog.error),
                Style::default().fg(theme::WARN),
            )));
        }
        f.render(Paragraph::new(lines), inner);
    }
}

pub fn draw_image(f: &mut Frame, area: Rect, img: &mut ImagePane) {
    let widget = StatefulImage::new().resize(Resize::Fit(None));
    f.render_stateful(widget, area, &mut img.proto);
}

pub fn draw_help(f: &mut Frame, area: Rect) {
    let w = area.width.saturating_sub(4).min(72);
    let h = area.height.saturating_sub(2).min(19);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    f.render(Clear, rect);
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(Line::from(vec![
            Span::styled(
                " termland ",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("keys · mouse", Style::default().fg(theme::FG_DIM)),
        ]))
        .style(Style::default().bg(theme::BG));
    let inner = block.inner(rect);
    f.render(block, rect);

    let key = |k: &'static str| Span::styled(k, Style::default().fg(theme::ACCENT));
    let d = |t: &'static str| Span::styled(t, Style::default().fg(theme::FG_DIM));
    let hi = |t: &'static str| {
        Span::styled(
            t,
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        )
    };
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  SUPER ",
                Style::default().fg(theme::ACCENT_FG).bg(theme::ACCENT),
            ),
            Span::styled(
                " the desktop key          ",
                Style::default().fg(theme::FG_DIM),
            ),
            Span::styled(
                " LEADER ",
                Style::default().fg(theme::ACCENT_FG).bg(theme::BG_ALT),
            ),
            d(" Ctrl+Space"),
        ]),
        Line::from(vec![
            key("   Q "),
            d("close pane      "),
            key("E "),
            d("file manager   "),
            key("   v | "),
            d("split side-by-side"),
        ]),
        Line::from(vec![
            key("   R "),
            d("app menu        "),
            key("V "),
            d("flip split     "),
            key("   s - "),
            d("split stacked"),
        ]),
        Line::from(vec![
            key("   F "),
            d("zoom pane       "),
            key("N "),
            d("new shell      "),
            key("   H J K L "),
            d("resize"),
        ]),
        Line::from(vec![
            key("   H J K L "),
            d("focus       "),
            key("⇧ H J K L "),
            d("move pane  "),
            key("   h j k l "),
            d("focus"),
        ]),
        Line::from(vec![
            key("   1…9 "),
            d("workspace    "),
            key("⇧ 1…9 "),
            d("send to ws  "),
            key("   Tab "),
            d("cycle panes"),
        ]),
        Line::from(vec![
            key("   D "),
            d("desktop         "),
            key("⇧ E "),
            d("files here     "),
            key("   G "),
            d("graphs"),
        ]),
        Line::from(vec![
            key("   N "),
            d("new shell       "),
            key("W "),
            d("web page       "),
            key("   ? "),
            d("this help"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  MOUSE ",
                Style::default().fg(theme::ACCENT_FG).bg(theme::ACCENT),
            ),
            d(" click focuses · "),
            hi("×"),
            d(" closes · drag a divider to resize"),
        ]),
        Line::from(vec![d(
            "         wheel scrolls · drag selects · right-click for the file menu",
        )]),
        Line::from(vec![d(
            "         children that ask for the mouse (btop, hx, nmtui) get real clicks",
        )]),
        Line::from(vec![d(
            "         on the desktop: click selects, a second click opens, Esc leaves",
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  FILE MANAGER  ", Style::default().fg(theme::FG_DIM)),
            d("Enter open · "),
            hi("r"),
            d(" rename · "),
            hi("n"),
            d(" folder · "),
            hi("d"),
            d(" delete · "),
            hi("."),
            d(" hidden · "),
            hi("q"),
            d(" close"),
        ]),
        Line::from(""),
        Line::from(vec![
            key("   tlc "),
            d("split v · files ~/Shows · run btop · image a.png · web url · list"),
        ]),
    ];
    f.render(Paragraph::new(lines), inner);
}

/// Draw the app launcher menu (start menu style) and return one hit rect per
/// app row, so a row can be clicked instead of only typed.
pub fn draw_menu(f: &mut Frame, area: Rect, app: &App) -> Vec<(Rect, String)> {
    let w = area.width.saturating_sub(4).min(70);
    let h = area.height.saturating_sub(4).min(24);
    let rect = Rect {
        x: area.x + 2,
        y: area.y + 2,
        width: w,
        height: h,
    };
    f.render(Clear, rect);
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(Line::from(vec![
            Span::styled(
                " termland ",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" apps ", Style::default().fg(theme::FG_DIM)),
        ]))
        .style(Style::default().bg(theme::BG));
    let inner = block.inner(rect);
    f.render(block, rect);

    let key = |k: &'static str| Span::styled(k, Style::default().fg(theme::ACCENT));
    let d = |t: String| Span::styled(t, Style::default().fg(theme::FG_DIM));

    let mut lines: Vec<Line> = vec![Line::from("")];
    // Line index of every clickable app row, in draw order.
    let mut rows: Vec<(usize, String)> = Vec::new();

    // Built-ins the desktop ships with, before anything the user installed.
    lines.push(Line::from(vec![Span::styled(
        "  Built in",
        Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
    )]));
    for (id, name, desc, chord) in crate::app::BUILTINS {
        rows.push((lines.len(), id.to_string()));
        lines.push(Line::from(vec![
            key("   • "),
            d(format!("{:<20} {}", name, desc)),
            d(format!("  {chord}")),
        ]));
    }
    lines.push(Line::from(""));

    // Group apps by category.
    for category in app.apps.categories() {
        lines.push(Line::from(vec![Span::styled(
            format!("  {category}"),
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        )]));
        for app_def in app.apps.by_category(category) {
            let desc = app_def.description.as_deref().unwrap_or("");
            let cmd_short = app_def.command.chars().take(30).collect::<String>();
            let line = if cmd_short.len() < app_def.command.len() {
                format!("{cmd_short}…")
            } else {
                cmd_short
            };
            rows.push((lines.len(), app_def.name.clone()));
            lines.push(Line::from(vec![
                key("   • "),
                d(format!("{:<20} {}", app_def.name, line)),
                d(format!("  {desc}")),
            ]));
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![
        Span::styled("  q  ", Style::default().fg(theme::ACCENT)),
        d("Close this menu".to_string()),
    ]));

    f.render(Paragraph::new(lines), inner);

    // The keyboard's row. It is painted *after* the text, so the glyphs stay
    // and only the background moves — a highlighted line, not a repainted one.
    let highlight = theme::blend(theme::BG, theme::ACCENT, 0.22);
    if let Some((li, _)) = rows.get(app.menu_sel) {
        let y = inner.y + *li as u16;
        if y < inner.y + inner.height {
            let buf = f.buffer_mut();
            for x in inner.x..inner.x + inner.width {
                let cell = &mut buf[(x, y)];
                cell.set_bg(highlight);
                cell.set_fg(theme::FG);
            }
        }
    }

    rows.into_iter()
        .filter(|(i, _)| *i < inner.height as usize)
        .map(|(i, name)| {
            (
                Rect {
                    x: inner.x,
                    y: inner.y + i as u16,
                    width: inner.width,
                    height: 1,
                },
                name,
            )
        })
        .collect()
}

fn conv(c: vt100::Color) -> ratatui::style::Color {
    use ratatui::style::Color;
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn conv_bg(c: vt100::Color) -> ratatui::style::Color {
    use ratatui::style::Color;
    match c {
        vt100::Color::Default => theme::BG,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::wrap_name;

    #[test]
    fn a_short_name_stays_on_one_row() {
        // 12 characters in a 14-wide row is one row. Balancing would split it
        // in the middle of the word for no reason at all.
        assert_eq!(wrap_name("keyboard.jpg", 14, 2), vec!["keyboard.jpg"]);
    }

    #[test]
    fn a_name_that_needs_two_rows_is_balanced_and_breaks_where_it_can() {
        assert_eq!(
            wrap_name("HYPRLAND-TRY.txt", 14, 2),
            vec!["HYPRLAND-", "TRY.txt"],
            "the dash is the closest break to an even split"
        );
        assert_eq!(
            wrap_name("keyboard.jpg", 10, 2),
            vec!["keyboard.", "jpg"],
            "a narrow row still breaks at the extension"
        );
        assert_eq!(
            wrap_name("tyre-prices-2026-09-26.csv", 14, 2),
            vec!["tyre-prices-", "2026-09-26.csv"],
            "break at the dash, and the extension survives"
        );
    }

    #[test]
    fn no_name_ever_prints_wider_than_its_row() {
        for name in [
            "averyveryveryverylongfilename.tar.gz",
            "x",
            "three words in a name.txt",
            "…already-shortened",
        ] {
            for width in 3..24usize {
                for lines in 1..3usize {
                    let out = wrap_name(name, width, lines);
                    assert!(out.len() <= lines, "{name} at {width}x{lines}: {out:?}");
                    for line in &out {
                        assert!(
                            line.chars().count() <= width,
                            "{name} at {width}x{lines}: {line:?} is too wide"
                        );
                    }
                }
            }
        }
        // Room for one row only, and a name that does not fit in it: the row
        // says so instead of pretending.
        let one = wrap_name("averyveryveryverylongfilename.tar.gz", 10, 2);
        assert!(one[1].ends_with('…'), "{one:?}");
    }
}
