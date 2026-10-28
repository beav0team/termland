//! Pane bookkeeping, the leader keymap, and the `tlc` command surface.

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui_image::picker::Picker;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::anim::Clock;
use crate::apps::AppRegistry;
use crate::config::Config;
use crate::desktop::{Desktop, DesktopAction};
use crate::files::{FileAction, FilePane};
use crate::image::ImagePane;
use crate::keys;
use crate::layout::{auto_dir, Dir, NavDir, Node, PaneId};
use crate::monitor::MonitorPane;
use crate::msg::{IpcRequest, Msg};
use crate::panel::{Panel, PanelAction};
use crate::pty::{shell_argv, TermPane};
use crate::surface::{Frame, Surface};
use crate::system::SystemInfo;
use crate::ui;
use crate::wallpaper::Wallpaper;
use crate::widgets::{Id, Pointer, Ui};

pub enum Pane {
    Term(TermPane),
    Image(ImagePane),
    /// A terminal running a text browser — a distinct kind only so the header
    /// can say so; it is a `TermPane` underneath.
    Web(TermPane),
    /// The built-in file manager: no child process, no pty, its own state.
    Files(FilePane),
    /// A live chart. Same shape as the file manager: no pty, its own state,
    /// drawn with the desktop's own renderer.
    Chart(MonitorPane),
}

impl Pane {
    pub fn kind(&self) -> &'static str {
        match self {
            Pane::Term(_) => "term",
            Pane::Image(_) => "image",
            Pane::Web(_) => "web",
            Pane::Files(_) => "files",
            Pane::Chart(_) => "chart",
        }
    }

    pub fn title(&self) -> String {
        match self {
            Pane::Term(t) | Pane::Web(t) => t.title(),
            Pane::Image(i) => i.label.clone(),
            Pane::Files(f) => f.title(),
            Pane::Chart(m) => m.title(),
        }
    }

    pub fn files(&self) -> Option<&FilePane> {
        match self {
            Pane::Files(f) => Some(f),
            _ => None,
        }
    }

    pub fn chart(&self) -> Option<&MonitorPane> {
        match self {
            Pane::Chart(m) => Some(m),
            _ => None,
        }
    }

    pub fn chart_mut(&mut self) -> Option<&mut MonitorPane> {
        match self {
            Pane::Chart(m) => Some(m),
            _ => None,
        }
    }

    pub fn files_mut(&mut self) -> Option<&mut FilePane> {
        match self {
            Pane::Files(f) => Some(f),
            _ => None,
        }
    }

    pub fn term(&self) -> Option<&TermPane> {
        match self {
            Pane::Term(t) | Pane::Web(t) => Some(t),
            Pane::Image(_) | Pane::Files(_) | Pane::Chart(_) => None,
        }
    }

    pub fn term_mut(&mut self) -> Option<&mut TermPane> {
        match self {
            Pane::Term(t) | Pane::Web(t) => Some(t),
            Pane::Image(_) | Pane::Files(_) | Pane::Chart(_) => None,
        }
    }
}

#[derive(Default)]
pub struct Workspace {
    pub layout: Option<Node>,
    pub focus: Option<PaneId>,
}

/// State for a drag-to-resize gesture: simply records that a drag is active.
#[derive(Clone, Copy, Debug)]
pub struct DragState {
    pub active: bool,
    pub start_x: u16,
    pub start_y: u16,
}

/// The panes the shell ships with, as the app menu lists them: `(id, name,
/// description, chord)`.
///
/// They are here and not in `apps.toml` because they are not programs — they are
/// pane types that cannot be launched any other way, and a launcher that cannot
/// open its own file manager is a launcher for other people's programs.
pub const BUILTINS: [(&str, &str, &str, &str); 6] = [
    ("files", "File manager", "browse this machine", "Super+E"),
    ("terminal", "Terminal", "a shell, right here", "Super+N"),
    ("image", "Image", "picture viewer", "leader i"),
    ("web", "Web page", "w3m, in a pane", "Super+W"),
    ("monitor", "Monitor", "live system graphs", "Super+G"),
    ("desktop", "Desktop", "wallpaper and icons", "Super+D"),
];

pub struct App {
    pub panes: HashMap<PaneId, Pane>,
    pub ws: Vec<Workspace>,
    pub cur: usize,
    pub zoom: Option<PaneId>,
    pub leader: bool,
    pub help: bool,
    pub quit: bool,
    pub status: String,
    pub area: Rect,
    pub tx: Sender<Msg>,
    pub picker: Option<Picker>,
    pub config: Config,
    pub apps: AppRegistry,
    pub panel: Panel,
    /// Wallpaper and the icons of `~/Desktop`.
    pub desktop: Desktop,
    /// The picture behind the desktop, if one was chosen.
    pub wallpaper: Wallpaper,
    pub sys: SystemInfo,
    pub surface: Box<dyn Surface>,
    /// Ticks since startup; drives the cursor blink without a second clock.
    ticks: u64,
    /// Whether the cursor is currently in its visible half of the blink.
    blink_on: bool,
    /// Monotonic time, for animation and for a blink that does not care how
    /// often the loop happens to wake up.
    pub clock: Clock,
    /// Pointer state: hover, press, hover fades.
    pub ui: Ui,
    next_id: PaneId,
    drag: Option<DragState>,
    menu_open: bool,
    /// Row of the app menu under the keyboard, so it can be used without a
    /// pointer. Reset every time the menu opens.
    pub menu_sel: usize,
    /// Close-box rects recorded while drawing the pane headers. Hit areas are
    /// never computed twice: whatever was drawn is what is clickable.
    header_close: Vec<(PaneId, Rect)>,
    /// App-menu rows recorded while drawing, so a row can be clicked.
    menu_hits: Vec<(Rect, String)>,
}

impl App {
    pub fn new(tx: Sender<Msg>, picker: Option<Picker>, surface: Box<dyn Surface>) -> Self {
        let full = surface.size();
        // Panel row, one blank row under it, and the panes take the rest.
        let area = Rect {
            y: full.y.saturating_add(2),
            height: full.height.saturating_sub(2),
            ..full
        };
        let (config, config_error) = Config::load_reporting();
        let (apps, apps_error) = AppRegistry::load_reporting();
        let panel_area = Rect::new(0, 0, area.width, area.height.saturating_sub(1));
        Self {
            panes: HashMap::new(),
            ws: vec![Workspace::default()],
            cur: 0,
            zoom: None,
            leader: false,
            help: false,
            quit: false,
            status: config_error.or(apps_error).unwrap_or_default(),
            area: panel_area,
            tx,
            picker,
            config,
            apps,
            panel: Panel::new(),
            desktop: Desktop::new(),
            wallpaper: Wallpaper::load(),
            // Cheap fields only: the expensive probes arrive on the channel a
            // moment later, so the first frame is never held up by nmcli.
            sys: SystemInfo {
                clock: crate::system::local_clock(),
                ..Default::default()
            },
            surface,
            ticks: 0,
            blink_on: true,
            clock: Clock::new(),
            ui: Ui::new(),
            next_id: 1,
            drag: None,
            menu_open: false,
            menu_sel: 0,
            header_close: Vec::new(),
            menu_hits: Vec::new(),
        }
    }

    // ---------------------------------------------------------------- queries

    /// Whether the desktop is what the user is looking at: asked for, or the
    /// workspace it would be drawn on has nothing tiled on it.
    pub fn desktop_visible(&self) -> bool {
        self.desktop.shows(!self.visible_rects().is_empty())
    }

    pub fn focus(&self) -> Option<PaneId> {
        self.ws.get(self.cur).and_then(|w| w.focus)
    }

    fn layout(&self) -> Option<&Node> {
        self.ws.get(self.cur).and_then(|w| w.layout.as_ref())
    }

    fn layout_mut(&mut self) -> Option<&mut Node> {
        self.ws.get_mut(self.cur).and_then(|w| w.layout.as_mut())
    }

    pub fn visible_rects(&self) -> Vec<(PaneId, Rect)> {
        if let Some(z) = self.zoom {
            if self.panes.contains_key(&z) {
                return vec![(z, self.pane_area())];
            }
        }
        match self.layout() {
            Some(n) => n.rects(self.pane_area()),
            None => Vec::new(),
        }
    }

    /// The area available for panes. `area` is already the body — below the
    /// panel, above nothing — so this is the whole of it.
    pub fn pane_area(&self) -> Rect {
        self.area
    }

    fn focus_rect(&self) -> Option<Rect> {
        let id = self.focus()?;
        self.visible_rects()
            .into_iter()
            .find(|(p, _)| *p == id)
            .map(|(_, r)| r)
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    // ------------------------------------------------------------ pane creation

    fn alloc_id(&mut self) -> PaneId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Insert an already-built pane next to the focused one.
    fn insert_pane(&mut self, pane: Pane, dir: Dir) -> PaneId {
        let id = self.alloc_id();
        self.panes.insert(id, pane);

        let ws = &mut self.ws[self.cur];
        if ws.layout.is_none() {
            ws.layout = Some(Node::leaf(id));
            ws.focus = Some(id);
        } else {
            let target = ws
                .focus
                .or_else(|| ws.layout.as_ref().and_then(|n| n.leaves().first().copied()));
            let split_ok = match target {
                Some(t) => ws
                    .layout
                    .as_mut()
                    .map(|root| root.split_leaf(t, id, dir, 0.5))
                    .unwrap_or(false),
                None => false,
            };
            if split_ok {
                ws.focus = Some(id);
            } else {
                // Nothing splittable (tiny terminal, or focus lost): the pane
                // still has to exist somewhere, so it becomes the whole area.
                ws.layout = Some(Node::leaf(id));
                ws.focus = Some(id);
            }
        }
        self.zoom = None;
        self.resize_all();
        id
    }

    pub fn spawn_term(&mut self, argv: Vec<String>, dir: Dir) -> Option<PaneId> {
        let (rows, cols) = self
            .focus_rect()
            .map(|r| (r.height.saturating_sub(1).max(2), r.width.max(2)))
            .unwrap_or((24, 80));
        let id = self.next_id;
        match TermPane::spawn(id, (rows, cols), argv, self.tx.clone()) {
            Ok(t) => {
                let pane = Pane::Term(t);
                Some(self.insert_pane(pane, dir))
            }
            Err(e) => {
                self.status = format!("spawn failed: {e}");
                None
            }
        }
    }

    pub fn spawn_web(&mut self, url: &str, dir: Dir) -> Option<PaneId> {
        let Some(browser) = find_prog(&["w3m", "lynx", "elinks", "links2"]) else {
            self.status = "no text browser found (install w3m)".into();
            return None;
        };
        let argv = vec![browser, url.to_string()];
        let (rows, cols) = self
            .focus_rect()
            .map(|r| (r.height.saturating_sub(1).max(2), r.width.max(2)))
            .unwrap_or((24, 80));
        let id = self.next_id;
        match TermPane::spawn(id, (rows, cols), argv, self.tx.clone()) {
            Ok(t) => Some(self.insert_pane(Pane::Web(t), dir)),
            Err(e) => {
                self.status = format!("web pane failed: {e}");
                None
            }
        }
    }

    pub fn spawn_image(&mut self, path: Option<PathBuf>, dir: Dir) -> Option<PaneId> {
        let Some(picker) = self.picker.as_ref() else {
            self.status = "no graphics picker; run inside kitty".into();
            return None;
        };
        let pane = match path {
            Some(p) => match ImagePane::from_path(picker, &p) {
                Ok(i) => i,
                Err(e) => {
                    self.status = format!("image: {e}");
                    return None;
                }
            },
            None => ImagePane::demo(picker),
        };
        Some(self.insert_pane(Pane::Image(pane), dir))
    }

    /// A live chart pane. Nothing is forked: the source is `/proc`, and the
    /// only thing this needs from the rest of the desktop is a rect.
    pub fn spawn_chart(&mut self, dir: Dir) -> PaneId {
        self.insert_pane(Pane::Chart(MonitorPane::new()), dir)
    }

    // ------------------------------------------------------------ pane actions

    pub fn split(&mut self, dir: Option<Dir>) -> Option<PaneId> {
        let rect = self.focus_rect()?;
        let dir = dir.unwrap_or_else(|| auto_dir(rect));
        let need = match dir {
            Dir::Horizontal => rect.width >= 6,
            Dir::Vertical => rect.height >= 4,
        };
        if !need {
            self.status = "pane too small to split".into();
            return None;
        }
        self.spawn_term(shell_argv(), dir)
    }

    pub fn close_focused(&mut self) {
        if let Some(id) = self.focus() {
            self.close_pane(id);
        }
    }

    pub fn close_pane(&mut self, id: PaneId) {
        let ws = &mut self.ws[self.cur];
        let leaves = ws.layout.as_ref().map(|n| n.leaves()).unwrap_or_default();
        if leaves.is_empty() {
            return;
        }
        // Focus the next leaf in reading order, the way a tiler does.
        let idx = leaves.iter().position(|p| *p == id);
        let next = idx.and_then(|i| {
            if leaves.len() > 1 {
                Some(leaves[(i + 1) % leaves.len()])
            } else {
                None
            }
        });

        let removed = if leaves.len() == 1 {
            ws.layout = None;
            true
        } else {
            ws.layout
                .as_mut()
                .map(|r| r.remove_leaf(id))
                .unwrap_or(false)
        };
        if !removed {
            return;
        }
        self.panes.remove(&id);
        if self.zoom == Some(id) {
            self.zoom = None;
        }
        if ws.focus == Some(id) {
            ws.focus =
                next.or_else(|| ws.layout.as_ref().and_then(|n| n.leaves().first().copied()));
        }
        self.resize_all();
    }

    pub fn focus_dir(&mut self, d: NavDir) {
        if self.zoom.is_some() {
            return;
        }
        let Some(cur) = self.focus() else { return };
        let next = self.layout().and_then(|n| n.neighbor(self.area, cur, d));
        if let Some(n) = next {
            self.ws[self.cur].focus = Some(n);
        }
    }

    pub fn focus_cycle(&mut self, delta: isize) {
        if self.zoom.is_some() {
            return;
        }
        let Some(cur) = self.focus() else { return };
        let Some(leaves) = self.layout().map(|n| n.leaves()) else {
            return;
        };
        if leaves.len() < 2 {
            return;
        }
        let i = leaves.iter().position(|p| *p == cur).unwrap_or(0) as isize;
        let n = leaves.len() as isize;
        let next = leaves[(((i + delta) % n + n) % n) as usize];
        self.ws[self.cur].focus = Some(next);
    }

    pub fn resize_focused(&mut self, d: NavDir, delta: f32) {
        let Some(cur) = self.focus() else { return };
        let sign = match d {
            NavDir::Right | NavDir::Down => delta,
            NavDir::Left | NavDir::Up => -delta,
        };
        if let Some(root) = self.ws[self.cur].layout.as_mut() {
            root.resize_along(cur, d.axis(), sign);
        }
        self.resize_all();
    }

    pub fn toggle_zoom(&mut self) {
        self.zoom = match (self.zoom, self.focus()) {
            (Some(_), _) => None,
            (None, Some(f)) => Some(f),
            (None, None) => None,
        };
        self.resize_all();
    }

    /// Move to a workspace without the "an empty workspace should have a
    /// shell" convenience. Startup uses this: the configured apps are about to
    /// fill the workspace, and spawning a shell first left every workspace
    /// with a stray bash pane the config never asked for.
    pub fn goto_workspace(&mut self, idx: usize) {
        if idx > 8 {
            return;
        }
        while self.ws.len() <= idx {
            self.ws.push(Workspace::default());
        }
        self.cur = idx;
        self.zoom = None;
        self.help = false;
    }

    pub fn switch_workspace(&mut self, idx: usize) {
        if idx > 8 {
            return;
        }
        self.goto_workspace(idx);
        // An empty workspace is a *desktop*, not a shell. It used to spawn one
        // on the way in, which meant the wallpaper and the icons had nowhere to
        // be: every workspace had a window on it before you could see it. A
        // shell is one key away, and not having one is a place you can be.
        self.resize_all();
    }

    /// Recompute every pty size from the current geometry. Called after any
    /// structural change and on every host resize.
    pub fn resize_all(&mut self) {
        for (id, r) in self.visible_rects() {
            if let Some(t) = self.panes.get_mut(&id).and_then(|p| p.term_mut()) {
                t.resize(r.height.saturating_sub(1).max(1), r.width.max(1));
            }
        }
    }

    // ------------------------------------------------------------- event entry

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Input(ev) => self.on_event(ev),
            Msg::Output(id, data) => {
                if let Some(t) = self.panes.get_mut(&id).and_then(|p| p.term_mut()) {
                    t.feed(&data);
                }
            }
            Msg::Exited(id) => {
                if let Some(t) = self.panes.get_mut(&id).and_then(|p| p.term_mut()) {
                    t.exited = true;
                }
            }
            Msg::Tick => self.on_tick(),
            Msg::Sys(info) => {
                // The clock in the snapshot may be a fraction stale; keep the
                // tick's value, which is the one the user is watching.
                let clock = std::mem::take(&mut self.sys.clock);
                self.sys = *info;
                if !clock.is_empty() {
                    self.sys.clock = clock;
                }
            }
            Msg::Ipc(req, reply) => {
                let out = self.on_ipc(req);
                let _ = reply.send(out);
            }
        }
    }

    fn on_tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        self.sys.clock = crate::system::local_clock();
        // Half a second on, half a second off — from the clock, not from the
        // loop beat: the loop now runs at 60 fps while anything animates, and
        // a blink counted in ticks would strobe.
        let _ = self.clock.tick();
        self.blink_on = blink_at(self.clock.elapsed());
        // Chart panes sample on their own schedule (1 Hz) while the loop runs
        // at 60 Hz; the chart animates between samples rather than stepping.
        for pane in self.panes.values_mut() {
            if let Pane::Chart(m) = pane {
                m.tick();
            }
        }
    }

    /// True while something on screen is still moving, which is when the loop
    /// should run at the frame rate instead of idling.
    pub fn animating(&self) -> bool {
        self.ui.animating()
            || self
                .panes
                .values()
                .any(|p| p.chart().is_some_and(|m| m.animating()))
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(ev) if ev.kind != KeyEventKind::Release => {
                if self.help {
                    self.help = false;
                    return;
                }
                // A context menu or dialog is modal: no Super, no leader, and
                // above all no bytes on their way to a shell.
                if self.file_modal() {
                    self.on_file_key(ev);
                    return;
                }
                if self.menu_open && !ev.modifiers.contains(KeyModifiers::SUPER) {
                    self.on_menu_key(ev);
                    return;
                }
                if !self.leader && ev.modifiers.contains(KeyModifiers::SUPER) {
                    self.on_super(ev);
                } else if self.leader {
                    self.leader = false;
                    self.on_leader(ev);
                } else if self.is_leader_key(&ev) {
                    self.leader = true;
                } else if self.desktop_visible() {
                    self.on_desktop_key(ev);
                } else if !self.on_chart_key(ev) && !self.on_file_key(ev) {
                    self.forward_key(ev);
                }
            }
            Event::Paste(text) => {
                let bytes = encode_paste(&text);
                self.forward_bytes(&bytes);
            }
            Event::Mouse(ev) => self.on_mouse(ev),
            Event::Resize(_, _) => self.resize_all(),
            _ => {}
        }
    }

    /// Check whether `ev` matches the configured leader key.
    fn is_leader_key(&self, ev: &KeyEvent) -> bool {
        let (m, c) = self.config.leader_key();
        ev.modifiers.contains(m) && ev.code == c
    }

    fn forward_key(&mut self, ev: KeyEvent) {
        if let Some(bytes) = keys::encode(ev) {
            self.forward_bytes(&bytes);
        }
    }

    fn forward_bytes(&mut self, bytes: &[u8]) {
        if let Some(id) = self.focus() {
            if let Some(t) = self.panes.get_mut(&id).and_then(|p| p.term_mut()) {
                t.write(bytes);
            }
        }
    }

    /// Mouse routing, in priority order: the panel, an open menu, a pane's
    /// close box, an open file menu, a divider, and finally the pane itself —
    /// which either gets its own clicks (file manager) or has them encoded and
    /// forwarded to whatever program is running in it.
    fn on_mouse(&mut self, ev: MouseEvent) {
        self.ui.set_pointer(Some((ev.column, ev.row)));
        match ev.kind {
            // Motion is what hover is made of. It is also the event a pane with
            // mouse tracking wants most, so it is forwarded only while the
            // pointer is inside a pane that asked for it.
            MouseEventKind::Moved => {
                if let Some((id, _)) = self.pane_under_rect(ev.column, ev.row) {
                    let wants = self
                        .panes
                        .get(&id)
                        .and_then(|p| p.term())
                        .map(|t| t.mouse.any_motion)
                        .unwrap_or(false);
                    if wants {
                        self.forward_mouse(id, ev);
                    }
                }
            }
            MouseEventKind::Down(_) => self.on_press(ev),
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.drag.is_some_and(|d| d.active) {
                    let area = self.area;
                    if let Some(layout) = self.layout_mut() {
                        layout.resize_divider_at(area, ev.column, ev.row);
                        self.resize_all();
                    }
                } else if let Some((id, _)) = self.pane_under_rect(ev.column, ev.row) {
                    self.forward_mouse(id, ev);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.is_some_and(|d| d.active) {
                    self.drag = None;
                } else if let Some(id) = self.focus() {
                    self.forward_mouse(id, ev);
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if self.desktop_visible() => {
                let area = self.area;
                let action = self.desktop.on_mouse(ev, area);
                self.apply_desktop_action(action);
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => self.on_wheel(ev),
            _ => {}
        }
    }

    fn pane_under_rect(&self, x: u16, y: u16) -> Option<(PaneId, Rect)> {
        self.visible_rects()
            .into_iter()
            .find(|(_, r)| contains(*r, x, y))
    }

    fn on_press(&mut self, ev: MouseEvent) {
        let (cx, cy) = (ev.column, ev.row);

        // Top panel.
        if cy == 0 {
            if let Some(action) = self.panel.hit_test(cx, cy) {
                self.on_panel_action(action);
                return;
            }
        }
        // The app menu is modal-ish: a click either picks a row or dismisses.
        if self.menu_open {
            let hit = self
                .menu_hits
                .iter()
                .find(|(r, _)| contains(*r, cx, cy))
                .map(|(_, name)| name.clone());
            self.menu_open = false;
            if let Some(name) = hit {
                self.launch_app(&name);
            }
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        // A pane's close box, left of its title.
        if ev.kind == MouseEventKind::Down(MouseButton::Left) {
            let close = self
                .header_close
                .iter()
                .find(|(_, r)| contains(*r, cx, cy))
                .map(|(id, _)| *id);
            if let Some(id) = close {
                self.close_pane(id);
                self.header_close.clear();
                return;
            }
        }
        // A file manager's context menu or dialog takes the click first: it
        // may hang over a pane that is not its own.
        if self.on_file_overlay(ev) {
            return;
        }
        // The desktop owns every click on it, including clicks on nothing.
        if self.desktop_visible() {
            let area = self.area;
            let action = self.desktop.on_mouse(ev, area);
            self.apply_desktop_action(action);
            return;
        }
        // A divider starts a drag-to-resize — but never at the expense of a
        // header. Tiles are adjacent, so the boundary row of a stacked split is
        // also the header row of the pane below it; taking that as a resize
        // handle made the header unclickable and the title un-focusable.
        let on_header_row = self
            .visible_rects()
            .iter()
            .any(|(_, r)| cy == r.y && cx >= r.x && cx < r.x.saturating_add(r.width));
        if ev.kind == MouseEventKind::Down(MouseButton::Left) && !on_header_row {
            if let Some(layout) = self.layout() {
                if layout.divider_at_point(self.area, cx, cy).is_some() {
                    self.drag = Some(DragState {
                        active: true,
                        start_x: cx,
                        start_y: cy,
                    });
                    return;
                }
            }
        }
        let Some((id, rect)) = self.pane_under_rect(cx, cy) else {
            return;
        };
        self.ws[self.cur].focus = Some(id);
        self.zoom = None;

        let content = Rect {
            x: rect.x,
            y: rect.y.saturating_add(1),
            width: rect.width,
            height: rect.height.saturating_sub(1),
        };
        if self.panes.get(&id).and_then(|p| p.files()).is_some() {
            let screen = self.screen_rect();
            let (action, status) = {
                let Some(Pane::Files(fp)) = self.panes.get_mut(&id) else {
                    return;
                };
                (fp.on_mouse(ev, content, screen), fp.status.clone())
            };
            if !status.is_empty() {
                self.status = status;
            }
            self.apply_file_action(action);
            return;
        }
        if let Some(Pane::Chart(m)) = self.panes.get_mut(&id) {
            m.on_wheel(matches!(ev.kind, MouseEventKind::ScrollUp));
            return;
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some(Pane::Chart(m)) = self.panes.get_mut(&id) {
                if m.on_click(cx, cy) {
                    return;
                }
            }
        }
        self.forward_mouse(id, ev);
    }

    /// True while a file manager menu or dialog is up, which also means it is
    /// the thing any click belongs to.
    fn on_file_overlay(&mut self, ev: MouseEvent) -> bool {
        let open: Option<PaneId> = self
            .panes
            .iter()
            .find(|(_, p)| {
                p.files()
                    .map(|f| f.menu.is_some() || f.dialog.is_some())
                    .unwrap_or(false)
            })
            .map(|(id, _)| *id);
        let Some(id) = open else { return false };
        let screen = self.screen_rect();
        let (action, status) = {
            let Some(Pane::Files(fp)) = self.panes.get_mut(&id) else {
                return false;
            };
            (fp.on_mouse(ev, screen, screen), fp.status.clone())
        };
        if !status.is_empty() {
            self.status = status;
        }
        self.apply_file_action(action);
        true
    }

    fn on_wheel(&mut self, ev: MouseEvent) {
        if self.file_modal() {
            return;
        }
        let Some((id, rect)) = self.pane_under_rect(ev.column, ev.row) else {
            return;
        };
        let content = Rect {
            x: rect.x,
            y: rect.y.saturating_add(1),
            width: rect.width,
            height: rect.height.saturating_sub(1),
        };
        if self.panes.get(&id).and_then(|p| p.files()).is_some() {
            let screen = self.screen_rect();
            let (action, status) = {
                let Some(Pane::Files(fp)) = self.panes.get_mut(&id) else {
                    return;
                };
                (fp.on_mouse(ev, content, screen), fp.status.clone())
            };
            if !status.is_empty() {
                self.status = status;
            }
            self.apply_file_action(action);
            return;
        }
        let tracking = self
            .panes
            .get(&id)
            .and_then(|p| p.term())
            .map(|t| t.mouse.tracking)
            .unwrap_or(false);
        if tracking {
            // The child wants the wheel (a pager, a monitor); it knows what
            // its own scrollback means better than termland does.
            self.forward_mouse(id, ev);
        } else if let Some(t) = self.panes.get_mut(&id).and_then(|p| p.term_mut()) {
            t.scroll(if ev.kind == MouseEventKind::ScrollUp {
                -1
            } else {
                1
            });
        }
    }

    /// Handle a click on the top panel.
    fn on_panel_action(&mut self, action: PanelAction) {
        match action {
            PanelAction::Menu => {
                self.menu_open = !self.menu_open;
                self.menu_sel = 0;
            }
            PanelAction::Wifi => {
                if let Some(app) = self.apps.get("nmtui") {
                    let argv = AppRegistry::expand_command(&app.command);
                    self.spawn_term(argv, Dir::Horizontal);
                } else {
                    self.status = "nmtui app not found".to_string();
                }
            }
            PanelAction::Volume => {
                // An audio mixer has to live inside the terminal: opening a GTK
                // `pavucontrol` from a TUI-only session is a dead end on a
                // machine with no X/Wayland, which is where this is headed.
                if let Some(mixer) = find_prog(&["pulsemixer", "alsamixer"]) {
                    self.spawn_term(vec![mixer], Dir::Horizontal);
                } else {
                    self.status = "no mixer found (install pulsemixer or alsamixer)".to_string();
                }
            }
            PanelAction::Bluetooth => {
                if let Some(app) = self.apps.get("bluetui") {
                    let argv = AppRegistry::expand_command(&app.command);
                    self.spawn_term(argv, Dir::Horizontal);
                } else {
                    self.status = "bluetui app not found".to_string();
                }
            }
            PanelAction::Terminal => {
                self.spawn_term(shell_argv(), Dir::Horizontal);
            }
            PanelAction::Workspace(n) => {
                self.switch_workspace(n);
            }
            PanelAction::Focus(id) => {
                if self.panes.contains_key(&id) {
                    self.ws[self.cur].focus = Some(id);
                    self.zoom = None;
                }
            }
        }
    }

    fn on_leader(&mut self, ev: KeyEvent) {
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        let key = match ev.code {
            KeyCode::Char(c) => c.to_ascii_lowercase(),
            KeyCode::Esc => return,
            _ => match ev.code {
                KeyCode::Right => return self.focus_dir(NavDir::Right),
                KeyCode::Left => return self.focus_dir(NavDir::Left),
                KeyCode::Up => return self.focus_dir(NavDir::Up),
                KeyCode::Down => return self.focus_dir(NavDir::Down),
                KeyCode::Tab => return self.focus_cycle(1),
                KeyCode::BackTab => return self.focus_cycle(-1),
                _ => return,
            },
        };

        if shift {
            return match key {
                'h' => self.resize_focused(NavDir::Left, 0.05),
                'j' => self.resize_focused(NavDir::Down, 0.05),
                'k' => self.resize_focused(NavDir::Up, 0.05),
                'l' => self.resize_focused(NavDir::Right, 0.05),
                _ => {}
            };
        }

        match key {
            'v' | '|' => {
                self.split(Some(Dir::Horizontal));
            }
            's' | '-' => {
                self.split(Some(Dir::Vertical));
            }
            'h' => self.focus_dir(NavDir::Left),
            'j' => self.focus_dir(NavDir::Down),
            'k' => self.focus_dir(NavDir::Up),
            'l' => self.focus_dir(NavDir::Right),
            ']' => self.focus_cycle(1),
            '[' => self.focus_cycle(-1),
            'n' => {
                self.spawn_term(
                    shell_argv(),
                    auto_dir(self.focus_rect().unwrap_or(self.area)),
                );
            }
            'i' => {
                self.spawn_image(None, Dir::Horizontal);
            }
            'w' => {
                self.spawn_web("https://duckduckgo.com", Dir::Horizontal);
            }
            'q' | 'x' | 'd' => self.close_focused(),
            'f' | 'z' => self.toggle_zoom(),
            '?' => self.help = true,
            '1'..='9' => {
                let n = key.to_digit(10).unwrap_or(1) as usize;
                self.switch_workspace(n - 1);
            }
            '0' => self.switch_workspace(9),
            ' ' => self.forward_bytes(&[0]),
            _ => {}
        }
    }

    // ------------------------------------------------------------- the desktop

    /// Open the built-in file manager, optionally at a directory.
    pub fn spawn_files(&mut self, path: Option<PathBuf>, dir: Dir) -> Option<PaneId> {
        let fp = FilePane::new(path);
        Some(self.insert_pane(Pane::Files(fp), dir))
    }

    /// Reveal the file manager with the cursor on `path`'s folder.
    pub fn spawn_files_at(&mut self, path: PathBuf) -> Option<PaneId> {
        let dir = auto_dir(self.focus_rect().unwrap_or(self.area));
        self.spawn_files(Some(path), dir)
    }

    /// Open a file in the place it belongs: images get an image pane, text gets
    /// the editor available on this machine.
    pub fn open_path(&mut self, path: PathBuf) {
        let dir = auto_dir(self.focus_rect().unwrap_or(self.area));
        self.open_path_in(path, dir);
    }

    fn open_path_in(&mut self, path: PathBuf, dir: Dir) {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tiff"
        ) {
            self.spawn_image(Some(path), dir);
            return;
        }
        let Some(editor) = find_prog(&["hx", "nvim", "vim", "vi", "nano", "less", "more"]) else {
            self.status = "no editor found (install hx, vim or nano)".into();
            return;
        };
        self.spawn_term(vec![editor, path.to_string_lossy().to_string()], dir);
    }

    fn apply_file_action(&mut self, action: FileAction) {
        match action {
            FileAction::None => {}
            FileAction::Open(path, is_dir) => {
                if is_dir {
                    self.spawn_files(Some(path), auto_dir(self.focus_rect().unwrap_or(self.area)));
                } else {
                    self.open_path(path);
                }
            }
            FileAction::OpenInPane(path, is_dir) => {
                let dir = auto_dir(self.focus_rect().unwrap_or(self.area));
                if is_dir {
                    self.spawn_files(Some(path), dir);
                } else {
                    self.open_path_in(path, dir);
                }
            }
            FileAction::ContextMenu => {
                let Some(id) = self.focus() else { return };
                let Some(rect) = self
                    .visible_rects()
                    .into_iter()
                    .find(|(p, _)| *p == id)
                    .map(|(_, r)| r)
                else {
                    return;
                };
                let content = Rect {
                    x: rect.x,
                    y: rect.y.saturating_add(1),
                    width: rect.width,
                    height: rect.height.saturating_sub(1),
                };
                if let Some(Pane::Files(fp)) = self.panes.get_mut(&id) {
                    let row = fp.row_of(fp.selected).unwrap_or(0);
                    let x = content.x.saturating_add(4);
                    let y = content
                        .y
                        .saturating_add(row)
                        .saturating_add(1)
                        .min(content.y.saturating_add(content.height.saturating_sub(1)));
                    fp.open_menu(x, y);
                }
            }
            FileAction::SetWallpaper(path) => self.use_wallpaper(path),
            FileAction::ClosePane => self.close_focused(),
        }
    }

    /// Adopt a picture as the wallpaper, and show it.
    ///
    /// Showing it is the point: the wallpaper is only visible on the desktop,
    /// so a menu item that set it and stayed behind a pane would look like it
    /// had done nothing at all.
    fn use_wallpaper(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        match self.wallpaper.set(&path) {
            Ok(()) => {
                self.status = format!("wallpaper · {name}");
                self.desktop.visible = true;
            }
            Err(e) => self.status = e,
        }
    }

    /// Launch a registry app by name, from the menu or a key.
    pub fn launch_app(&mut self, name: &str) {
        let dir = auto_dir(self.focus_rect().unwrap_or(self.area));
        match name {
            "files" => {
                self.spawn_files(None, dir);
                return;
            }
            "terminal" => {
                self.spawn_term(shell_argv(), dir);
                return;
            }
            "image" => {
                self.spawn_image(None, dir);
                return;
            }
            "web" => {
                self.spawn_web("https://duckduckgo.com", dir);
                return;
            }
            "monitor" => {
                self.spawn_chart(dir);
                return;
            }
            "desktop" => {
                self.toggle_desktop();
                return;
            }
            _ => {}
        }
        let argv = self.apps.argv_for(name);
        if argv.is_empty() {
            self.status = format!("no such app: {name}");
            return;
        }
        self.spawn_term(argv, dir);
    }

    /// Swap the focused pane with the neighbour in `d` — Hyprland's
    /// `Super+Shift+h/j/k/l`, which reorders windows without moving them.
    pub fn swap_focused(&mut self, d: NavDir) {
        if self.zoom.is_some() {
            return;
        }
        let Some(cur) = self.focus() else { return };
        let next = self.layout().and_then(|n| n.neighbor(self.area, cur, d));
        let Some(next) = next else { return };
        if let Some(root) = self.ws[self.cur].layout.as_mut() {
            if root.swap_leaves(cur, next) {
                self.ws[self.cur].focus = Some(next);
            }
        }
        self.resize_all();
    }

    /// Flip the focused pane's parent between side-by-side and stacked.
    pub fn toggle_split(&mut self) {
        let Some(cur) = self.focus() else { return };
        if let Some(root) = self.ws[self.cur].layout.as_mut() {
            root.toggle_dir_of(cur);
        }
        self.resize_all();
    }

    /// Send the focused pane to another workspace without following it, the
    /// way `Super+Shift+<n>` does in Hyprland.
    pub fn move_focused_to_workspace(&mut self, idx: usize) {
        if idx > 8 || idx == self.cur {
            return;
        }
        let Some(id) = self.focus() else { return };

        // Detach from the current workspace but keep the pane alive.
        {
            let ws = &mut self.ws[self.cur];
            let leaves = ws.layout.as_ref().map(|n| n.leaves()).unwrap_or_default();
            if leaves.is_empty() {
                return;
            }
            let pos = leaves.iter().position(|p| *p == id);
            let next = pos.and_then(|i| {
                if leaves.len() > 1 {
                    Some(leaves[(i + 1) % leaves.len()])
                } else {
                    None
                }
            });
            let removed = if leaves.len() == 1 {
                ws.layout = None;
                true
            } else {
                ws.layout
                    .as_mut()
                    .map(|r| r.remove_leaf(id))
                    .unwrap_or(false)
            };
            if !removed {
                return;
            }
            if ws.focus == Some(id) {
                ws.focus =
                    next.or_else(|| ws.layout.as_ref().and_then(|n| n.leaves().first().copied()));
            }
        }

        while self.ws.len() <= idx {
            self.ws.push(Workspace::default());
        }
        {
            let target = &mut self.ws[idx];
            match target.layout.as_mut() {
                Some(root) => {
                    let at = target
                        .focus
                        .filter(|p| root.contains(*p))
                        .or_else(|| root.leaves().first().copied());
                    let split_ok = at
                        .map(|t| root.split_leaf(t, id, Dir::Horizontal, 0.5))
                        .unwrap_or(false);
                    if split_ok {
                        target.focus = Some(id);
                    } else {
                        // Nowhere to split (a one-column workspace): the pane
                        // still has to land somewhere rather than being lost.
                        let old = std::mem::replace(root, Node::leaf(id));
                        *root = Node::Split {
                            dir: Dir::Horizontal,
                            ratio: 0.5,
                            first: Box::new(old),
                            second: Box::new(Node::leaf(id)),
                        };
                        target.focus = Some(id);
                    }
                }
                None => {
                    target.layout = Some(Node::leaf(id));
                    target.focus = Some(id);
                }
            }
        }
        self.zoom = None;
        self.resize_all();
        self.status = format!("pane moved to workspace {}", idx + 1);
    }

    // --------------------------------------------------------------- input bits

    /// A file pane's context menu or dialog is modal: until it is answered no
    /// key is a tile command, and no byte goes to a shell.
    fn file_modal(&self) -> bool {
        self.focus()
            .and_then(|id| self.panes.get(&id))
            .and_then(|p| p.files())
            .map(|f| f.dialog.is_some() || f.menu.is_some())
            .unwrap_or(false)
    }

    /// The app menu, by keyboard. It is a list of six built-ins and whatever is
    /// installed: clicking it is fine, but a launcher you can only click is a
    /// launcher that does not belong in a keyboard-driven desktop.
    fn on_menu_key(&mut self, ev: KeyEvent) {
        let last = self.menu_hits.len().saturating_sub(1);
        match ev.code {
            KeyCode::Esc | KeyCode::Char('q') => self.menu_open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.menu_sel = self.menu_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu_sel = (self.menu_sel + 1).min(last);
            }
            KeyCode::Home | KeyCode::Char('g') => self.menu_sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.menu_sel = last,
            KeyCode::Enter | KeyCode::Char(' ') => {
                let name = self
                    .menu_hits
                    .get(self.menu_sel)
                    .map(|(_, n)| n.clone())
                    .or_else(|| self.menu_hits.first().map(|(_, n)| n.clone()));
                self.menu_open = false;
                if let Some(name) = name {
                    self.launch_app(&name);
                }
            }
            _ => {}
        }
    }

    /// Route a key to the focused file pane. Returns false when the focused
    /// pane is not a file manager, so the caller can send it to a pty.
    /// Keys a chart pane owns: the digits pick a kind, +/- zoom.
    fn on_chart_key(&mut self, ev: KeyEvent) -> bool {
        let Some(id) = self.focus() else { return false };
        if self.panes.get(&id).and_then(|p| p.chart()).is_none() {
            return false;
        }
        match self.panes.get_mut(&id) {
            Some(Pane::Chart(m)) => m.on_key(ev),
            _ => false,
        }
    }

    fn on_file_key(&mut self, ev: KeyEvent) -> bool {
        let Some(id) = self.focus() else { return false };
        if self.panes.get(&id).and_then(|p| p.files()).is_none() {
            return false;
        }
        let (action, status) = {
            let Some(Pane::Files(fp)) = self.panes.get_mut(&id) else {
                return false;
            };
            (fp.on_key(ev), fp.status.clone())
        };
        if !status.is_empty() {
            self.status = status;
        }
        self.apply_file_action(action);
        true
    }

    fn screen_rect(&self) -> Rect {
        Rect {
            x: self.area.x,
            y: self.area.y,
            width: self.area.width,
            height: self.area.height.saturating_add(1),
        }
    }

    /// Super is the desktop key, the way it is in Hyprland. Which of these
    /// actually reach termland depends on the host: a running compositor grabs
    /// its own `Super` bindings first, but inside termland's own session (boot
    /// to it, no compositor) every one of them arrives.
    /// Draw the desktop layer and register its controls for hover, so a tile
    /// lights up under the pointer exactly like a file-manager tile does.
    fn draw_desktop(&mut self, frame: &mut Frame, body: Rect) {
        self.desktop.set_viewport(body);
        let hovered = self
            .ui
            .pointer()
            .and_then(|(px, py)| self.desktop.hit(body, px, py));
        let hover = match hovered {
            Some(idx) => {
                let id = Id::new("desktop-tile", idx);
                let fade = self.ui.fade(id, 0.0);
                if let Some(rect) = self.desktop.tile_rect(body, idx) {
                    self.ui.hit(id, rect);
                }
                fade
            }
            None => 0.0,
        };
        ui::draw_desktop(
            frame,
            body,
            &self.desktop,
            hovered,
            hover,
            &mut self.wallpaper,
        );
        ui::draw_desktop_menu(frame, &self.desktop, body);
    }

    /// Resolve what the desktop asked for. Directories get a file manager; a
    /// file goes wherever its type belongs.
    fn apply_desktop_action(&mut self, action: DesktopAction) {
        if !self.desktop.status.is_empty() {
            self.status = std::mem::take(&mut self.desktop.status);
        }
        match action {
            DesktopAction::None => {}
            DesktopAction::Hide => self.desktop.visible = false,
            DesktopAction::SetWallpaper(path) => self.use_wallpaper(path),
            DesktopAction::Open(path) => {
                self.desktop.visible = false;
                if path.is_dir() {
                    let dir = auto_dir(self.focus_rect().unwrap_or(self.area));
                    self.spawn_files(Some(path), dir);
                } else {
                    self.open_path(path);
                }
            }
        }
    }

    fn on_desktop_key(&mut self, ev: KeyEvent) {
        let area = self.area;
        let action = self.desktop.on_key(ev, area);
        self.apply_desktop_action(action);
    }

    /// Show or hide the desktop over the current workspace.
    pub fn toggle_desktop(&mut self) {
        self.desktop.visible = !self.desktop.visible;
        if self.desktop.visible {
            self.desktop.reload();
            self.status = format!("desktop · {}", self.desktop.dir.display());
        } else {
            self.status.clear();
        }
    }

    fn on_super(&mut self, ev: KeyEvent) {
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        let area_dir = |s: &Self| s.focus_rect().map(auto_dir).unwrap_or(auto_dir(s.area));
        match ev.code {
            KeyCode::Char(c) => {
                if let Some(n) = number_of(c) {
                    if shift {
                        self.move_focused_to_workspace(n.saturating_sub(1));
                    } else {
                        self.switch_workspace(n.saturating_sub(1));
                    }
                    return;
                }
                let c = c.to_ascii_lowercase();
                if shift {
                    match c {
                        'h' => self.swap_focused(NavDir::Left),
                        'j' => self.swap_focused(NavDir::Down),
                        'k' => self.swap_focused(NavDir::Up),
                        'l' => self.swap_focused(NavDir::Right),
                        'e' => {
                            let dir = area_dir(self);
                            self.spawn_files(None, dir);
                        }
                        'q' => self.quit = true,
                        _ => {}
                    }
                    return;
                }
                match c {
                    // Hyprland's Super+Q: kill the active window.
                    'q' | 'x' => self.close_focused(),
                    // Hyprland's Super+E: file manager.
                    'e' => {
                        let dir = area_dir(self);
                        self.spawn_files(None, dir);
                    }
                    // Super+R: the launcher menu (Hyprland binds a menu here).
                    'r' => {
                        self.menu_open = !self.menu_open;
                        self.menu_sel = 0;
                    }
                    // Super+D: the desktop, as on every other desktop there is.
                    'd' => self.toggle_desktop(),
                    // Super+Return: a terminal, as in Hyprland's default config.
                    'm' => self.quit = true,
                    'v' => self.toggle_split(),
                    'f' | 'z' => self.toggle_zoom(),
                    'h' => self.focus_dir(NavDir::Left),
                    'j' => self.focus_dir(NavDir::Down),
                    'k' => self.focus_dir(NavDir::Up),
                    'l' => self.focus_dir(NavDir::Right),
                    'n' => {
                        let dir = area_dir(self);
                        self.spawn_term(shell_argv(), dir);
                    }
                    'w' => {
                        let dir = area_dir(self);
                        self.spawn_web("https://duckduckgo.com", dir);
                    }
                    // Super+G: graphs. The capability, surfaced.
                    'g' => {
                        let dir = area_dir(self);
                        self.spawn_chart(dir);
                    }
                    '=' | '+' => self.resize_focused(NavDir::Right, 0.05),
                    '-' => self.resize_focused(NavDir::Left, 0.05),
                    '?' => self.help = true,
                    ' ' => self.focus_cycle(1),
                    _ => {}
                }
            }
            KeyCode::Enter => {
                let dir = area_dir(self);
                self.spawn_term(shell_argv(), dir);
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                let d = match ev.code {
                    KeyCode::Left => NavDir::Left,
                    KeyCode::Right => NavDir::Right,
                    KeyCode::Up => NavDir::Up,
                    _ => NavDir::Down,
                };
                if shift {
                    self.swap_focused(d);
                } else {
                    self.focus_dir(d);
                }
            }
            KeyCode::Tab => self.focus_cycle(1),
            KeyCode::Esc => self.menu_open = false,
            _ => {}
        }
    }

    /// Forward a mouse event to a child that asked for mouse reporting. The
    /// coordinates are rebased onto the pane's own screen, which is the only
    /// thing the child knows about.
    fn forward_mouse(&mut self, id: PaneId, ev: MouseEvent) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let Some(t) = pane.term() else { return };
        if !t.mouse.tracking {
            return;
        }
        let Some(rect) = self
            .visible_rects()
            .into_iter()
            .find(|(p, _)| *p == id)
            .map(|(_, r)| r)
        else {
            return;
        };
        let content = Rect {
            x: rect.x,
            y: rect.y.saturating_add(1),
            width: rect.width,
            height: rect.height.saturating_sub(1),
        };
        let modes = t.mouse;
        let Some(bytes) = crate::mouse::encode(
            &ev,
            modes,
            (content.x, content.y),
            (content.width, content.height),
        ) else {
            return;
        };
        self.write_to(id, &bytes);
    }

    fn write_to(&mut self, id: PaneId, bytes: &[u8]) {
        if let Some(t) = self.panes.get_mut(&id).and_then(|p| p.term_mut()) {
            t.write(bytes);
        }
    }

    // --------------------------------------------------------------------- ipc

    fn on_ipc(&mut self, req: IpcRequest) -> String {
        let args = &req.args;
        match req.cmd.as_str() {
            "ping" => "pong\n".into(),
            "split" => {
                let dir = match args.first().map(|s| s.as_str()) {
                    Some("v") | Some("h") | Some("horizontal") | Some("right") | Some("r") => {
                        Some(Dir::Horizontal)
                    }
                    Some("s") | Some("j") | Some("vertical") | Some("down") | Some("d") => {
                        Some(Dir::Vertical)
                    }
                    _ => None,
                };
                match self.split(dir) {
                    Some(id) => format!("ok {id}\n"),
                    None => "error could not split\n".into(),
                }
            }
            "close" => {
                self.close_focused();
                "ok\n".into()
            }
            "focus" => {
                match args.first().map(|s| s.as_str()) {
                    Some("h") | Some("left") => self.focus_dir(NavDir::Left),
                    Some("j") | Some("down") => self.focus_dir(NavDir::Down),
                    Some("k") | Some("up") => self.focus_dir(NavDir::Up),
                    Some("l") | Some("right") => self.focus_dir(NavDir::Right),
                    Some("next") => self.focus_cycle(1),
                    Some("prev") => self.focus_cycle(-1),
                    _ => {}
                }
                match self.focus() {
                    Some(id) => format!("ok {id}\n"),
                    None => "error no panes\n".into(),
                }
            }
            "resize" => {
                match args.first().map(|s| s.as_str()) {
                    Some("h") | Some("left") => self.resize_focused(NavDir::Left, 0.05),
                    Some("j") | Some("down") => self.resize_focused(NavDir::Down, 0.05),
                    Some("k") | Some("up") => self.resize_focused(NavDir::Up, 0.05),
                    Some("l") | Some("right") => self.resize_focused(NavDir::Right, 0.05),
                    _ => {}
                }
                "ok\n".into()
            }
            "zoom" => {
                self.toggle_zoom();
                "ok\n".into()
            }
            "files" => {
                let path = args.first().filter(|s| !s.is_empty()).map(PathBuf::from);
                match self.spawn_files(path, Dir::Horizontal) {
                    Some(id) => format!("ok {id}\n"),
                    None => "error files failed\n".into(),
                }
            }
            "shell" => match self.spawn_term(shell_argv(), Dir::Horizontal) {
                Some(id) => format!("ok {id}\n"),
                None => "error spawn failed\n".into(),
            },
            "run" => {
                if args.is_empty() {
                    return "error run needs a program\n".into();
                }
                match self.spawn_term(args.clone(), Dir::Horizontal) {
                    Some(id) => format!("ok {id}\n"),
                    None => "error spawn failed\n".into(),
                }
            }
            "image" => {
                let path = args.first().filter(|s| !s.is_empty()).map(PathBuf::from);
                match self.spawn_image(path, Dir::Horizontal) {
                    Some(id) => format!("ok {id}\n"),
                    None => "error image failed\n".into(),
                }
            }
            "web" => {
                let url = args
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "https://duckduckgo.com".into());
                match self.spawn_web(&url, Dir::Horizontal) {
                    Some(id) => format!("ok {id}\n"),
                    None => "error web failed\n".into(),
                }
            }
            "keys" => {
                let text = crate::ipc::decode_escapes(&args.join("\t"));
                self.forward_bytes(text.as_bytes());
                "ok\n".into()
            }
            "key" => {
                // A *tile* key chord, as the desktop itself would see it:
                // `tlc key super+e`, `tlc key n`, `tlc key ctrl+space`.
                let chord = args.first().map(|s| s.as_str()).unwrap_or("");
                match parse_chord(chord) {
                    Some(ev) => {
                        self.on_event(Event::Key(ev));
                        "ok\n".into()
                    }
                    None => format!("error cannot read key {chord}\n"),
                }
            }
            "workspace" => {
                let n: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
                self.switch_workspace(n.saturating_sub(1).min(9));
                format!("ok {}\n", self.cur + 1)
            }
            "list" => {
                let mut out = String::new();
                let focus = self.focus();
                for (id, r) in self.visible_rects() {
                    let Some(p) = self.panes.get(&id) else {
                        continue;
                    };
                    // The mouse column is what makes "why does clicking in
                    // btop do nothing" answerable from a shell.
                    let mouse = match p.term() {
                        Some(t) if t.mouse.tracking && t.mouse.sgr => "sgr",
                        Some(t) if t.mouse.tracking => "x10",
                        _ => "",
                    };
                    out.push_str(&format!(
                        "{}\t{}\t{}\t{}x{}\t{}\t{}\n",
                        id,
                        p.kind(),
                        p.title(),
                        r.width,
                        r.height,
                        if Some(id) == focus { "*" } else { "" },
                        mouse
                    ));
                }
                if out.is_empty() {
                    out.push_str("empty\n");
                }
                out
            }
            "layout" => format!(
                "ok {}x{} ws={} panes={}\n",
                self.area.width,
                self.area.height,
                self.cur + 1,
                self.panes.len()
            ),
            "quit" => {
                self.quit = true;
                "ok\n".into()
            }
            // What the pointer layer believes. Hover is invisible in a log, so
            // it has to be inspectable.
            "ui" => format!(
                "pointer={:?} hover={:?} pressed={:?} shape={:?} animating={}\n",
                self.ui.pointer(),
                self.ui.hover(),
                self.ui.pressed(),
                self.ui.pointer_shape(),
                self.animating()
            ),
            // Synthetic mouse events: the pointer cannot be moved from a
            // script on every compositor, but hover has to be testable.
            // The file pane's view, so the icon grid can be driven from a
            // script the way every other pane state can.
            "view" => {
                let Some(id) = self.focus() else {
                    return "error no pane\n".to_string();
                };
                let want = match args.first().map(|s| s.as_str()) {
                    Some("icons") | Some("grid") => super::files::FileView::Grid,
                    Some("list") => super::files::FileView::List,
                    _ => return "error view <list|icons>\n".to_string(),
                };
                let Some(Pane::Files(fp)) = self.panes.get_mut(&id) else {
                    return "error focused pane is not a file pane\n".to_string();
                };
                fp.set_view(want);
                let name = fp.view.name();
                self.draw();
                format!("ok {name}\n")
            }
            // The desktop, so a session can be started on it and a script can
            // put it away again.
            "desktop" => {
                // No argument is a question, not an instruction: "is it on?".
                match args.first().map(|s| s.as_str()) {
                    Some("on") => {
                        if !self.desktop.visible {
                            self.toggle_desktop();
                        }
                    }
                    None => {}
                    Some("off") => {
                        if self.desktop.visible {
                            self.toggle_desktop();
                        }
                    }
                    Some("toggle") => self.toggle_desktop(),
                    Some(other) => return format!("error desktop {other}\n"),
                }
                self.draw();
                // The honest answer is what is on screen, not what the flag
                // says: an empty workspace shows the desktop without it.
                format!("ok {}\n", if self.desktop_visible() { "on" } else { "off" })
            }
            // The wallpaper: `wallpaper <path>` sets it, `wallpaper clear`
            // goes back to the gradient, no argument asks what it is.
            "wallpaper" => match args.first().map(|s| s.as_str()) {
                None => match &self.wallpaper.path {
                    Some(p) => format!("ok {}\n", p.display()),
                    None => "ok none\n".into(),
                },
                Some("clear") | Some("none") => match self.wallpaper.clear() {
                    Ok(()) => {
                        self.status = "wallpaper cleared".into();
                        self.draw();
                        "ok none\n".into()
                    }
                    Err(e) => format!("error {e}\n"),
                },
                Some(path) => {
                    self.use_wallpaper(PathBuf::from(path));
                    self.draw();
                    match &self.wallpaper.path {
                        Some(p) => format!("ok {}\n", p.display()),
                        None => format!("error {}\n", self.status),
                    }
                }
            },
            // A live chart pane: the graph engine, with a system source.
            "chart" => {
                let dir = auto_dir(self.focus_rect().unwrap_or(self.area));
                let id = self.spawn_chart(dir);
                self.draw();
                format!("ok {id}\n")
            }
            "mouse" => {
                let parse = |i: usize| args.get(i).and_then(|v| v.parse::<u16>().ok()).ok_or(());
                let (Ok(x), Ok(y)) = (parse(0), parse(1)) else {
                    return "error usage: mouse <x> <y> [move|click|right]\n".into();
                };
                let what = args.get(2).map(|s| s.as_str()).unwrap_or("move");
                let kind = match what {
                    "move" => MouseEventKind::Moved,
                    "click" | "right" => MouseEventKind::Down(if what == "click" {
                        MouseButton::Left
                    } else {
                        MouseButton::Right
                    }),
                    other => return format!("error unknown mouse action {other}\n"),
                };
                let ev = MouseEvent {
                    kind,
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::empty(),
                };
                self.handle(Msg::Input(Event::Mouse(ev)));
                if what != "move" {
                    let up = MouseEvent {
                        kind: MouseEventKind::Up(MouseButton::Left),
                        ..ev
                    };
                    self.handle(Msg::Input(Event::Mouse(up)));
                }
                self.draw();
                "ok\n".into()
            }
            other => format!("error unknown command {other}\n"),
        }
    }

    // -------------------------------------------------------------------- draw

    pub fn draw(&mut self) {
        let mut frame = self.surface.begin_frame();
        // Hover is recomputed from geometry on every frame, in draw order, so
        // "what is on top" is decided by what was drawn last.
        self.ui.begin_frame();
        let full = frame.area();
        // One bar, at the top. The panes get one empty row between themselves
        // and it: a desktop where the chrome touches the first line of text
        // reads as a terminal, not as a desktop.
        let body = Rect {
            y: full.y.saturating_add(2),
            height: full.height.saturating_sub(2),
            ..full
        };
        let geometry_changed = body != self.area;
        self.area = body;
        if geometry_changed {
            self.resize_all();
        }
        ui::draw_background(&mut frame, full);

        // --- Top panel ---
        let panel_area = Rect {
            x: full.x,
            y: full.y,
            width: full.width,
            height: 1,
        };
        // Panel geometry is stable, so last frame's rects are the right thing
        // to hit-test against; this frame's rects are recorded for the next.
        let panel_hover = self
            .ui
            .pointer()
            .filter(|(_, y)| *y == panel_area.y)
            .and_then(|(x, _)| {
                self.panel
                    .buttons
                    .iter()
                    .position(|b| x >= b.x && x < b.x.saturating_add(b.width))
            });
        let buttons = Panel::draw(frame.buffer_mut(), panel_area, self, &self.sys, panel_hover);
        for (i, b) in buttons.iter().enumerate() {
            self.ui
                .hit(Id::new("panel", i), Rect::new(b.x, b.y, b.width, b.height));
        }
        self.panel.buttons = buttons;

        // Panes below the panel — or the desktop, when the desktop is the
        // thing there is to show. A tiling session has no "behind", so the two
        // are exclusive: either the workspace is tiled, or it is a desktop.
        let desktop = self.desktop_visible();
        let rects = if desktop {
            Vec::new()
        } else {
            self.visible_rects()
        };
        if desktop {
            self.draw_desktop(&mut frame, body);
        }
        let focus = self.focus();
        let mut cursor = None;
        let mut header_close: Vec<(PaneId, Rect)> = Vec::new();

        for (id, r) in rects {
            let focused = Some(id) == focus;
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            let header = Rect {
                height: r.height.min(1),
                ..r
            };
            let content = Rect {
                x: r.x,
                y: r.y.saturating_add(1),
                width: r.width,
                height: r.height.saturating_sub(1),
            };
            let close_id = Id::new("pane-close", id);
            let close_rect = header_close
                .iter()
                .find(|(p, _)| *p == id)
                .map(|(_, r)| *r)
                .or_else(|| {
                    self.header_close
                        .iter()
                        .find(|(p, _)| *p == id)
                        .map(|(_, r)| *r)
                });
            let close_fade = match close_rect {
                Some(r) if self.ui.hit(close_id, r) => self.ui.fade(close_id, 0.0),
                _ => self.ui.fade(close_id, 0.0),
            };
            header_close.push((
                id,
                ui::draw_header(&mut frame, header, pane, focused, close_fade),
            ));
            if content.height == 0 || content.width == 0 {
                continue;
            }
            match pane {
                Pane::Image(img) => ui::draw_image(&mut frame, content, img),
                Pane::Chart(m) => {
                    let pointer = self
                        .ui
                        .pointer()
                        .filter(|(px, py)| contains(content, *px, *py));
                    m.draw(frame.buffer_mut(), content, pointer);
                }
                Pane::Files(fp) => {
                    fp.set_viewport(content);
                    // One entry is one control: its id includes the pane and the
                    // entry index, so it survives the list scrolling and means
                    // the same thing in both views.
                    let hover_row = self
                        .ui
                        .pointer()
                        .and_then(|(px, py)| fp.hit(content, px, py));
                    let row_id = hover_row.map(|r| Id::new("file-row", (id, r)));
                    let hover = match row_id {
                        Some(rid) => self.ui.fade(rid, 0.0),
                        None => 0.0,
                    };
                    if let Some(rid) = row_id {
                        // The hover target is the row in list view and the whole
                        // tile in the icon view: the pointer is inside it either
                        // way, so the fade starts from the thing that is under it.
                        let rect = fp
                            .tile_rect(content, hover_row.unwrap_or(0))
                            .unwrap_or_else(|| {
                                Rect::new(
                                    content.x,
                                    self.ui.pointer().unwrap_or((0, 0)).1,
                                    content.width,
                                    1,
                                )
                            });
                        self.ui.hit(rid, rect);
                    }
                    ui::draw_files(&mut frame, content, fp, focused, hover_row, hover);
                }
                Pane::Term(t) | Pane::Web(t) => {
                    if let Some(c) = ui::draw_term(&mut frame, content, t, focused) {
                        cursor = Some(c);
                    }
                }
            }
        }
        self.header_close = header_close;

        // File menus and dialogs live above every pane: they may be anchored
        // near an edge and hang over a neighbour.
        let files_panes: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.files().is_some())
            .map(|(id, _)| *id)
            .collect();
        let screen = Rect {
            x: 0,
            y: 0,
            width: full.width,
            height: full.height,
        };
        for id in files_panes {
            if let Some(Pane::Files(fp)) = self.panes.get_mut(&id) {
                ui::draw_file_overlay(&mut frame, fp, screen);
            }
        }

        if self.menu_open {
            self.menu_hits = ui::draw_menu(&mut frame, body, self);
        }
        if self.help {
            ui::draw_help(&mut frame, body);
        }
        if let Some((x, y)) = cursor {
            // `Terminal::draw` hides the cursor unless a position is set, so a
            // blank half of the blink is genuinely hidden, not left behind.
            if self.blink_on {
                frame.set_cursor_position((x, y));
            }
        }
        // One place decides the pointer shape: after everything is drawn, so
        // "over a button" means the same thing to the cursor as to the click.
        let shape = self.pointer_shape();
        self.ui.resolve_pointer(shape);
        self.ui.end_frame();
        self.surface.set_pointer(shape);
        self.surface.present(frame);
    }

    /// What the cursor should look like, from geometry alone.
    fn pointer_shape(&self) -> Pointer {
        let Some((x, y)) = self.ui.pointer() else {
            return Pointer::Default;
        };
        if self.drag.is_some_and(|d| d.active) {
            return Pointer::Grabbing;
        }
        if y == 0 {
            return if self.panel.hit_test(x, y).is_some() {
                Pointer::Pointer
            } else {
                Pointer::Default
            };
        }
        if self.header_close.iter().any(|(_, r)| contains(*r, x, y)) {
            return Pointer::Pointer;
        }
        let on_header_row = self
            .visible_rects()
            .iter()
            .any(|(_, r)| y == r.y && x >= r.x && x < r.x.saturating_add(r.width));
        if !on_header_row {
            if let Some(layout) = self.layout() {
                if let Some((dir, _)) = layout.divider_at_point(self.area, x, y) {
                    return match dir {
                        Dir::Horizontal => Pointer::ResizeHorizontal,
                        Dir::Vertical => Pointer::ResizeVertical,
                    };
                }
            }
        }
        match self.pane_under_rect(x, y) {
            Some((id, rect)) => {
                // Inside a pane: text over a terminal grid, a hand over the
                // file manager's rows, an arrow over an image.
                let content = Rect {
                    y: rect.y.saturating_add(1),
                    height: rect.height.saturating_sub(1),
                    ..rect
                };
                if !contains(content, x, y) {
                    return Pointer::Default;
                }
                match self.panes.get(&id) {
                    Some(Pane::Term(_)) | Some(Pane::Web(_)) => Pointer::Text,
                    Some(Pane::Files(_)) => Pointer::Pointer,
                    Some(Pane::Chart(_)) => Pointer::Pointer,
                    _ => Pointer::Default,
                }
            }
            None => Pointer::Default,
        }
    }
}

/// Cursor blink: half a second visible, half a second not, computed from
/// elapsed time rather than from how many times the loop happened to wake up.
/// The loop runs at 60 fps while something animates and at 4 fps when idle, so
/// counting beats would make the cursor strobe.
pub fn blink_at(elapsed: f32) -> bool {
    (elapsed % 1.0) < 0.5
}

fn encode_paste(text: &str) -> Vec<u8> {
    // Bracketed paste markers are only sent when the child asked for them, so
    // plain text is the safe universal default.
    text.as_bytes().to_vec()
}

/// Read a chord like `super+q`, `ctrl+space` or `n` into a key event, for the
/// `tlc key` command. Unknown names are refused rather than guessed.
pub fn parse_chord(text: &str) -> Option<KeyEvent> {
    let mut mods = KeyModifiers::empty();
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        if let Some(tail) = lower
            .strip_prefix("super+")
            .or_else(|| lower.strip_prefix("meta+"))
        {
            mods |= KeyModifiers::SUPER;
            rest = &rest[rest.len() - tail.len()..];
        } else if let Some(tail) = lower.strip_prefix("shift+") {
            mods |= KeyModifiers::SHIFT;
            rest = &rest[rest.len() - tail.len()..];
        } else if let Some(tail) = lower
            .strip_prefix("ctrl+")
            .or_else(|| lower.strip_prefix("control+"))
        {
            mods |= KeyModifiers::CONTROL;
            rest = &rest[rest.len() - tail.len()..];
        } else if let Some(tail) = lower.strip_prefix("alt+") {
            mods |= KeyModifiers::ALT;
            rest = &rest[rest.len() - tail.len()..];
        } else {
            break;
        }
    }
    let code = match rest.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        other if other.starts_with('f') && other[1..].parse::<u8>().is_ok() => {
            KeyCode::F(other[1..].parse::<u8>().unwrap())
        }
        other => {
            let mut chars = other.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    };
    Some(KeyEvent::new(code, mods))
}

/// `1`…`9` from a key that may have arrived shifted: with the kitty keyboard
/// protocol `Shift+1` is `1` plus SHIFT, without it, it is `!`. Both have to
/// mean workspace one.
fn number_of(c: char) -> Option<usize> {
    match c {
        '1'..='9' => c.to_digit(10).map(|n| n as usize),
        '!' => Some(1),
        '@' => Some(2),
        '#' => Some(3),
        '$' => Some(4),
        '%' => Some(5),
        '^' => Some(6),
        '&' => Some(7),
        '*' => Some(8),
        '(' => Some(9),
        ')' => Some(10),
        _ => None,
    }
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x.saturating_add(r.width) && y >= r.y && y < r.y.saturating_add(r.height)
}

pub fn find_prog(names: &[&str]) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for n in names {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// Demo layout: a monitor top-left, a real editor bottom-left, and an image
/// pane filling the right column. Everything the project claims to do, on
/// screen at once.
pub fn build_demo(app: &mut App, image: Option<PathBuf>) {
    let monitor = find_prog(&["btop", "htop", "top"]).unwrap_or_else(|| "top".into());
    let readme = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../README.md")
        .canonicalize()
        .ok();

    // 1. monitor fills the window
    app.spawn_term(vec![monitor], Dir::Horizontal);
    // 2. image takes the right column
    app.spawn_image(image, Dir::Horizontal);
    // 3. back to the left column, cut it in two, edit the README underneath
    app.focus_dir(NavDir::Left);
    let mut argv = vec![find_prog(&["hx", "nvim", "vim", "vi"]).unwrap_or_else(|| "less".into())];
    if let Some(r) = readme {
        argv.push(r.to_string_lossy().to_string());
    }
    app.spawn_term(argv, Dir::Vertical);
    app.focus_dir(NavDir::Up);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::KittySurface;

    fn app() -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        App::new(tx, None, Box::new(KittySurface::default()))
    }

    use ratatui::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    fn press(app: &mut App, code: KeyCode) {
        app.handle(Msg::Input(Event::Key(KeyEvent::new(
            code,
            KeyModifiers::empty(),
        ))));
    }

    fn super_press(app: &mut App, code: KeyCode, shift: bool) {
        let mut mods = KeyModifiers::SUPER;
        if shift {
            mods |= KeyModifiers::SHIFT;
        }
        app.handle(Msg::Input(Event::Key(KeyEvent::new(code, mods))));
    }

    fn click(app: &mut App, x: u16, y: u16, button: MouseButton) {
        app.handle(Msg::Input(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(button),
            column: x,
            row: y,
            modifiers: KeyModifiers::empty(),
        })));
    }

    fn kinds(app: &App) -> Vec<&'static str> {
        app.visible_rects()
            .into_iter()
            .filter_map(|(id, _)| app.panes.get(&id).map(|p| p.kind()))
            .collect()
    }

    #[test]
    fn the_panel_lists_only_the_panes_on_the_current_workspace() {
        use crate::panel::PanelAction;
        let mut a = app();
        a.spawn_files(None, Dir::Horizontal);
        a.draw();
        let count = a
            .panel
            .buttons
            .iter()
            .filter(|b| matches!(b.action, PanelAction::Focus(_)))
            .count();
        assert_eq!(count, 1);

        // A pane on another workspace is not a window you can see or click.
        a.goto_workspace(1);
        a.spawn_files(None, Dir::Horizontal);
        a.draw();
        let count = a
            .panel
            .buttons
            .iter()
            .filter(|b| matches!(b.action, PanelAction::Focus(_)))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn shift_f10_opens_the_file_menu_where_the_selection_is() {
        let mut a = app();
        a.spawn_files(None, Dir::Horizontal);
        a.draw();
        let rect = a.visible_rects()[0].1;
        let ev = parse_chord("shift+f10").expect("parsed");
        assert_eq!(ev.code, KeyCode::F(10));
        a.handle(Msg::Input(Event::Key(ev)));
        let fp = a.panes.values().find_map(|p| p.files()).expect("file pane");
        let menu = fp.menu.as_ref().expect("menu opens");
        assert!(menu.y > rect.y, "the menu hangs off the selected row");
        assert!(menu.x >= rect.x);
    }

    #[test]
    fn a_header_row_is_clickable_even_when_it_is_the_divider_row() {
        // Two stacked panes: the boundary row is the second pane's header. The
        // header must win — otherwise the lower pane cannot be focused at all.
        let mut a = app();
        // A real window, then a file manager stacked under it.
        a.spawn_files(Some(std::env::temp_dir()), Dir::Horizontal);
        a.draw();
        a.spawn_files(Some(std::env::temp_dir()), Dir::Vertical);
        a.draw();
        let rects = a.visible_rects();
        assert_eq!(rects.len(), 2);
        let lower = rects[1].1;
        let (x, y) = (lower.x + 6, lower.y);
        assert!(
            a.layout()
                .and_then(|l| l.divider_at_point(a.area, x, y))
                .is_some(),
            "this row really is the divider row"
        );
        a.draw();
        assert!(
            !a.header_close.is_empty() || a.focus().is_some(),
            "the header is still drawn"
        );

        // A press on the header focuses the pane instead of starting a drag.
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::empty(),
        };
        a.handle(Msg::Input(Event::Mouse(ev)));
        assert!(a.drag.is_none(), "the header must not become a resize drag");
    }

    #[test]
    fn chords_are_read_the_way_a_desktop_writes_them() {
        let ev = parse_chord("super+q").expect("parsed");
        assert_eq!(ev.code, KeyCode::Char('q'));
        assert!(ev.modifiers.contains(KeyModifiers::SUPER));

        let ev = parse_chord("ctrl+space").expect("parsed");
        assert_eq!(ev.code, KeyCode::Char(' '));
        assert!(ev.modifiers.contains(KeyModifiers::CONTROL));

        assert_eq!(parse_chord("enter").unwrap().code, KeyCode::Enter);
        assert_eq!(parse_chord("esc").unwrap().code, KeyCode::Esc);
        assert!(parse_chord("nonsense+key+name").is_none());
    }

    #[test]
    fn super_e_opens_the_file_manager() {
        let mut a = app();
        a.draw();
        super_press(&mut a, KeyCode::Char('e'), false);
        assert_eq!(kinds(&a), vec!["files"]);
    }

    #[test]
    fn super_q_closes_the_focused_pane() {
        let mut a = app();
        a.draw();
        a.spawn_files(None, Dir::Horizontal);
        assert_eq!(a.pane_count(), 1);
        super_press(&mut a, KeyCode::Char('q'), false);
        assert_eq!(a.pane_count(), 0);
        assert!(a.visible_rects().is_empty());
    }

    #[test]
    fn the_close_box_on_the_left_closes_that_pane() {
        let mut a = app();
        a.spawn_files(None, Dir::Horizontal);
        a.spawn_files(None, Dir::Horizontal);
        a.draw();
        assert_eq!(a.header_close.len(), 2, "one close box per visible pane");
        // The close box is the first three cells of a pane's header row.
        let (_, close) = a.header_close[0];
        let before = a.pane_count();
        click(&mut a, close.x + 1, close.y, MouseButton::Left);
        assert_eq!(a.pane_count(), before - 1);
    }

    #[test]
    fn super_shift_moves_a_pane_into_the_neighbour_s_slot() {
        let mut a = app();
        a.spawn_files(None, Dir::Horizontal);
        a.spawn_files(None, Dir::Horizontal);
        let focus = a.focus().expect("a focused pane");
        let other = a
            .visible_rects()
            .into_iter()
            .find(|(id, _)| *id != focus)
            .map(|(id, _)| id)
            .expect("two panes");
        let rect_of = |app: &App, id: PaneId| {
            app.visible_rects()
                .into_iter()
                .find(|(p, _)| *p == id)
                .map(|(_, r)| r)
        };
        let focus_rect = rect_of(&a, focus);
        let other_rect = rect_of(&a, other);
        super_press(&mut a, KeyCode::Char('h'), true);
        assert_eq!(rect_of(&a, focus), other_rect);
        assert_eq!(rect_of(&a, other), focus_rect);
    }

    #[test]
    fn a_file_dialog_swallows_super_q_until_it_is_dismissed() {
        let mut a = app();
        a.spawn_files(None, Dir::Horizontal);
        a.draw();
        press(&mut a, KeyCode::Char('n'));
        assert!(a.file_modal());
        super_press(&mut a, KeyCode::Char('q'), false);
        assert_eq!(a.pane_count(), 1, "the pane must survive a modal");

        press(&mut a, KeyCode::Esc);
        assert!(!a.file_modal());
        super_press(&mut a, KeyCode::Char('q'), false);
        assert_eq!(a.pane_count(), 0);
    }

    #[test]
    fn right_click_in_the_file_manager_opens_its_menu() {
        let mut a = app();
        a.spawn_files(None, Dir::Horizontal);
        a.draw();
        let rect = a.visible_rects()[0].1;
        // Row 0 of the listing is "..", row 1 is the first real entry.
        click(&mut a, rect.x + 2, rect.y + 2, MouseButton::Right);
        let fp = a.panes.values().find_map(|p| p.files()).expect("file pane");
        assert!(fp.menu.is_some());
        assert_eq!(fp.selected, 1);
    }

    #[test]
    fn a_file_menu_is_closed_by_a_click_that_misses_it() {
        let mut a = app();
        a.spawn_files(None, Dir::Horizontal);
        a.draw();
        let rect = a.visible_rects()[0].1;
        click(&mut a, rect.x + 2, rect.y + 2, MouseButton::Right);
        assert!(a
            .panes
            .values()
            .any(|p| p.files().map(|f| f.menu.is_some()).unwrap_or(false)));
        // Far from the menu, in the same pane's listing.
        click(
            &mut a,
            rect.x + rect.width.saturating_sub(1),
            rect.y + rect.height.saturating_sub(1),
            MouseButton::Left,
        );
        assert!(a
            .panes
            .values()
            .all(|p| p.files().map(|f| f.menu.is_none()).unwrap_or(true)));
        assert_eq!(a.pane_count(), 1, "dismissing a menu is not a tile action");
    }

    #[test]
    fn super_plus_uses_the_shifted_forms_of_the_digits_too() {
        let mut a = app();
        a.spawn_files(None, Dir::Horizontal);
        // With the kitty protocol the code is '1' + SHIFT; without it, '!'.
        super_press(&mut a, KeyCode::Char('1'), true);
        assert_eq!(a.cur, 0, "workspace 1 keeps the pane");
        super_press(&mut a, KeyCode::Char('!'), true);
        assert_eq!(a.cur, 0);
        // Shift+2 sends the focused pane to workspace 2 instead.
        super_press(&mut a, KeyCode::Char('@'), true);
        assert!(a.ws[1].layout.is_some());
    }

    #[test]
    fn tick_refreshes_the_clock() {
        let mut a = app();
        a.sys.clock = "--:--".into();
        a.handle(Msg::Tick);
        // Not compared against a second `local_clock()` call: a minute
        // boundary between the two would make that flaky.
        assert_ne!(a.sys.clock, "--:--");
        let (h, m) = a.sys.clock.split_once(':').expect("HH:MM");
        assert!(h.parse::<u8>().unwrap() < 24);
        assert!(m.parse::<u8>().unwrap() < 60);
    }

    #[test]
    fn the_blink_is_half_a_second_of_clock_not_of_loop_beats() {
        // Whatever the loop rate, the cadence is the same in seconds.
        for (t, want) in [
            (0.0, true),
            (0.49, true),
            (0.5, false),
            (0.99, false),
            (1.0, true),
            (1.75, false),
        ] {
            assert_eq!(blink_at(t), want, "at {t}s");
        }
    }

    #[test]
    fn a_tick_advances_the_blink_clock() {
        let mut a = app();
        a.handle(Msg::Tick);
        assert!(a.blink_on, "a fresh app starts visible");
        assert!(!a.animating(), "an idle app must not ask for 60 fps");
    }

    #[test]
    fn a_sys_snapshot_does_not_roll_the_clock_back() {
        let mut a = app();
        a.handle(Msg::Tick);
        let clock = a.sys.clock.clone();
        let info = Box::new(SystemInfo {
            clock: "00:00".into(),
            ..Default::default()
        });
        a.handle(Msg::Sys(info));
        assert_eq!(a.sys.clock, clock);
        assert_eq!(a.sys.battery.percent, 0);
    }
}
