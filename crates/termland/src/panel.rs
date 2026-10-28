//! Top control panel — desktop-manager style status bar.
//!
//! Shows workspaces, open windows, clock, weather, wifi, bluetooth, battery,
//! volume, and a menu button. Clickable areas trigger actions via IPC commands
//! (nmtui for wifi, pulsemixer for volume, etc.).
//!
//! The panel is drawn above the pane area; `App` reserves the top row for it.

use crate::app::App;
use crate::system::SystemInfo;
use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

/// A clickable region on the panel.
#[derive(Clone, Copy, Debug)]
pub struct PanelButton {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub action: PanelAction,
}

/// What happens when a panel button is clicked.
#[derive(Clone, Copy, Debug)]
pub enum PanelAction {
    /// Open the wifi/network manager.
    Wifi,
    /// Open the audio mixer.
    Volume,
    /// Open bluetooth manager.
    Bluetooth,
    /// Open a terminal shell.
    Terminal,
    /// Switch to workspace N.
    Workspace(usize),
    /// Open the main menu (app launcher).
    Menu,
    /// Focus the pane whose title was clicked.
    Focus(crate::layout::PaneId),
}

pub struct Panel {
    pub buttons: Vec<PanelButton>,
    pub height: u16,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            buttons: Vec::new(),
            height: 1,
        }
    }

    /// Draw the panel into the buffer, returning the clickable regions.
    pub fn draw(
        buf: &mut Buffer,
        area: Rect,
        app: &App,
        sys: &SystemInfo,
        hover: Option<usize>,
    ) -> Vec<PanelButton> {
        let mut buttons = Vec::new();

        // Background: fill the whole area with BG_ALT style.
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                buf[(x, y)].set_style(Style::default().bg(theme::BG_ALT));
            }
        }

        // Air between segments, and air at both ends. The panel used to run
        // every label together and start flush against the window edge, which
        // reads as one long string rather than as separate controls.
        const GAP: u16 = 2;
        const INSET: u16 = 1;
        let mut cx = area.x.saturating_add(INSET);

        // Right side: system info. It is measured first, because the left side
        // has to stop before it: the task list used to be drawn straight
        // through the wifi icon.
        let right_segments: Vec<(String, Option<PanelAction>, Style)> = vec![
            (
                format!(" {} ", sys.wifi_icon()),
                Some(PanelAction::Wifi),
                Style::default().fg(theme::FG).bg(theme::BG_ALT),
            ),
            (
                format!(" {} ", sys.bluetooth_icon()),
                Some(PanelAction::Bluetooth),
                Style::default().fg(theme::FG).bg(theme::BG_ALT),
            ),
            (
                format!(" {}% ", sys.audio.percent),
                Some(PanelAction::Volume),
                Style::default().fg(theme::FG).bg(theme::BG_ALT),
            ),
            (
                format!(" {}% ", sys.battery.percent),
                None,
                Style::default().fg(theme::FG).bg(theme::BG_ALT),
            ),
            (
                format!(" {} ", sys.clock),
                None,
                Style::default()
                    .fg(theme::ACCENT_FG)
                    .bg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        let right_width: u16 = right_segments
            .iter()
            .map(|(s, _, _)| s.as_str().width() as u16)
            .sum::<u16>()
            + GAP * right_segments.len().saturating_sub(1) as u16
            + INSET;
        let mut rx = area
            .x
            .saturating_add(area.width)
            .saturating_sub(right_width);

        // The transient status message, between the task list and the system
        // block. It is the only thing that used to need a second bar, and it is
        // one line: clip it rather than let it shove the clock off the edge.
        let mut note = String::new();
        if app.zoom.is_some() {
            note.push_str(" ZOOM ");
        }
        if !app.status.is_empty() {
            note.push_str(&app.status);
        }
        let note_width = note.width() as u16;
        let (note, note_width) = if note_width > 0 && note_width + GAP < rx.saturating_sub(cx) {
            (note, note_width)
        } else {
            (String::new(), 0)
        };
        // Everything on the left stops here.
        let limit = rx.saturating_sub(if note_width > 0 { note_width + GAP } else { 0 });

        // Helper to draw a segment and register a button.
        let draw_segment = |cx: &mut u16,
                            text: &str,
                            style: Style,
                            action: Option<PanelAction>,
                            buf: &mut Buffer,
                            buttons: &mut Vec<PanelButton>,
                            area: Rect,
                            limit: u16| {
            let tw = text.width() as u16;
            if *cx + tw > limit {
                return;
            }
            buf.set_stringn(*cx, area.y, text, tw as usize, style);
            if let Some(a) = action {
                buttons.push(PanelButton {
                    x: *cx,
                    y: area.y,
                    width: tw,
                    height: 1,
                    action: a,
                });
            }
            *cx = cx.saturating_add(tw).saturating_add(GAP);
        };

        // Leader mode is modal, so it takes the whole bar: a second line at the
        // bottom of the window just to say "you are in a mode" was the reason
        // there were two bars.
        if app.leader {
            let hint = " LEADER ▸ v/s split · h j k l focus · n new · i image · w web · f zoom · q close · ? help ";
            buf.set_stringn(
                area.x,
                area.y,
                format!("{hint:<width$}", width = area.width as usize),
                area.width as usize,
                Style::default()
                    .fg(theme::ACCENT_FG)
                    .bg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            );
            return buttons;
        }

        // --- Menu button (leftmost) ---
        draw_segment(
            &mut cx,
            " ☰ ",
            Style::default()
                .fg(theme::ACCENT_FG)
                .bg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
            Some(PanelAction::Menu),
            buf,
            &mut buttons,
            area,
            limit,
        );

        // --- Workspaces ---
        let ws_count = app.ws.len().clamp(3, 9);
        for i in 0..ws_count {
            let active = i == app.cur;
            let used = app.ws.get(i).map(|w| w.layout.is_some()).unwrap_or(false);
            let label = format!(" {} ", i + 1);
            let style = if active {
                Style::default()
                    .fg(theme::ACCENT_FG)
                    .bg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else if used {
                Style::default().fg(theme::FG).bg(theme::BG_ALT)
            } else {
                Style::default().fg(theme::FG_DIM).bg(theme::BG_ALT)
            };
            draw_segment(
                &mut cx,
                &label,
                style,
                Some(PanelAction::Workspace(i)),
                buf,
                &mut buttons,
                area,
                limit,
            );
        }

        // Separator.
        draw_segment(
            &mut cx,
            " ▏ ",
            Style::default().fg(theme::BORDER).bg(theme::BG_ALT),
            None,
            buf,
            &mut buttons,
            area,
            limit,
        );

        // --- Open windows on *this* workspace, in layout order ---
        //
        // It used to walk `app.panes`, which is every pane on every workspace
        // in hash order: a second shell on workspace 4 showed up as a duplicate
        // of the focused window and could not be clicked. A taskbar lists what
        // is on screen, in the order it is on screen.
        if let Some(focus_id) = app.focus() {
            for (id, _) in app.visible_rects() {
                let Some(pane) = app.panes.get(&id) else {
                    continue;
                };
                let focused = id == focus_id;
                // A file pane's title is a path; a taskbar wants the last part
                // of it. `/home/yassine/Pictures` is a *picture* folder, and the
                // six segments in front of the word are not the label.
                let title = pane.title();
                let title = title
                    .rsplit('/')
                    .find(|seg| !seg.is_empty())
                    .map(str::to_string)
                    .unwrap_or(title);
                let truncated: String = if title.chars().count() > 20 {
                    // By chars, not bytes: a pane title can be a path with an
                    // accent in it, and `&title[..17]` would panic on the
                    // character boundary.
                    format!("{}…", title.chars().take(17).collect::<String>())
                } else {
                    title
                };
                let label = format!(" {} ", truncated);
                let style = if focused {
                    Style::default()
                        .fg(theme::ACCENT_FG)
                        .bg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::FG_DIM).bg(theme::BG_ALT)
                };
                draw_segment(
                    &mut cx,
                    &label,
                    style,
                    Some(PanelAction::Focus(id)),
                    buf,
                    &mut buttons,
                    area,
                    limit,
                );
            }
        }

        // The status message is right-aligned against the system block.
        if note_width > 0 {
            buf.set_stringn(
                rx.saturating_sub(note_width + GAP),
                area.y,
                &note,
                note_width as usize,
                Style::default().fg(theme::WARN).bg(theme::BG_ALT),
            );
        }

        // Draw right-aligned segments. The last one stops one cell short of the
        // edge, so the clock is a button on a bar rather than a full stop.
        let edge = area.x.saturating_add(area.width);
        for (text, action, style) in right_segments {
            draw_segment(&mut rx, &text, style, action, buf, &mut buttons, area, edge);
        }

        // The hovered segment is repainted last, over whatever style it drew
        // itself with: segments are not widgets, so there is no per-segment
        // style to look up here.
        if let Some(b) = hover.and_then(|i| buttons.get(i)).copied() {
            for x in b.x..b.x.saturating_add(b.width) {
                let cell = &mut buf[(x, area.y)];
                cell.set_style(Style::default().fg(theme::BG).bg(theme::ACCENT));
            }
        }

        buttons
    }

    /// Find the action for a click at (x, y), if any.
    pub fn hit_test(&self, x: u16, y: u16) -> Option<PanelAction> {
        self.buttons.iter().find_map(|b| {
            if x >= b.x && x < b.x.saturating_add(b.width) && y == b.y {
                Some(b.action)
            } else {
                None
            }
        })
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}
