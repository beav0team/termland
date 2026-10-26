//! The desktop: a wallpaper with the icons of `~/Desktop` on it.
//!
//! Every tiling desktop has this hole. Panes fill the screen, so there is no
//! "behind" for a wallpaper or an icon grid to live in — the desktop only
//! exists when there is nothing tiled, and then it is *the* window. That is the
//! whole design here: `visible` means "show me the desktop", the workspace
//! being empty means the same thing, and the two cannot disagree because both
//! go through [`Desktop::shows`].
//!
//! Icons come from the same [`Tiles`] geometry and the same half-block paint as
//! the file manager's icon view, because a folder looks the same wherever it
//! is and a second way of drawing one would be a second thing to keep correct.
//!
//! The right button opens the desktop's own menu — open, set a picture as the
//! wallpaper, reread the folder, leave — which is the one thing a desktop does
//! that a file manager cannot do for it.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::files::{sort_entries, Entry, Tiles};

const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// What the desktop asks the app to do. Resolving it is the app's job: the
/// desktop knows about files, not about panes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopAction {
    None,
    /// Open this path — the app decides whether that is a pane, a picture or
    /// an editor.
    Open(PathBuf),
    /// Use this picture as the wallpaper.
    SetWallpaper(PathBuf),
    /// Leave desktop mode.
    Hide,
}

/// A row of the desktop's context menu. Built from the selection, like the
/// file manager's: "Set as wallpaper" only exists for a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Open,
    SetWallpaper,
    Refresh,
    Leave,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Open => "Open",
            MenuItem::SetWallpaper => "Set as wallpaper",
            MenuItem::Refresh => "Refresh",
            MenuItem::Leave => "Leave desktop",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Menu {
    /// Where the pointer was, in screen cells.
    pub x: u16,
    pub y: u16,
    pub item: usize,
    /// Whether the click that opened it landed on an icon. A menu on the bare
    /// wallpaper must not offer to open whichever icon happened to be
    /// selected: the click said "here", not "that one".
    pub on_icon: bool,
}

pub struct Desktop {
    /// The folder the icons are the contents of.
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub offset: usize,
    pub tiles: Tiles,
    /// Icons that fit at the last known size. Kept because "keep the selection
    /// on screen" is asked from places that do not have the area to hand.
    pub capacity: usize,
    /// Show-desktop mode: the panes are hidden and this is the session.
    pub visible: bool,
    pub status: String,
    /// The right-click menu, if one is up.
    pub menu: Option<Menu>,
    last_click: Option<(Instant, usize)>,
}

impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}

impl Desktop {
    pub fn new() -> Self {
        let mut d = Self {
            dir: default_dir(),
            entries: Vec::new(),
            selected: 0,
            offset: 0,
            tiles: Tiles::for_area(Rect::new(0, 0, 80, 24)),
            capacity: 0,
            visible: false,
            status: String::new(),
            menu: None,
            last_click: None,
        };
        d.reload();
        d
    }

    /// Whether the desktop is what the user is looking at: explicitly asked
    /// for, or the workspace has nothing on it to look at instead.
    pub fn shows(&self, workspace_has_panes: bool) -> bool {
        self.visible || !workspace_has_panes
    }

    pub fn reload(&mut self) {
        // The menu names entries by position; a reread can move them.
        self.menu = None;
        self.entries.clear();
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let meta = match ent.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                self.entries.push(Entry {
                    name,
                    path: ent.path(),
                    is_dir: meta.is_dir(),
                    size: if meta.is_dir() { 0 } else { meta.len() },
                });
            }
        }
        sort_entries(&mut self.entries);
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
        self.clamp_offset();
    }

    /// Point the desktop at another folder. `~/Desktop` is the default, but a
    /// session that boots into a picture directory is not a mistake.
    pub fn set_dir(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.selected = 0;
        self.offset = 0;
        self.reload();
    }

    // ------------------------------------------------------------ geometry

    pub fn set_viewport(&mut self, area: Rect) {
        // Roomier than a file pane's grid: an icon you point at, not one you
        // scan in a column. Eighteen rather than sixteen because the names sit
        // edge to edge at sixteen: two 14-character filenames in neighbouring
        // tiles read as one run of text. The tile is the name's gutter.
        self.tiles = Tiles::for_width(area, 18);
        self.capacity = self.tiles.capacity(area);
        self.clamp_offset();
    }

    /// Keep the selection inside the grid: the offset follows it, and the
    /// offset itself never runs past the end of the list.
    pub fn clamp_offset(&mut self) {
        let cap = self.capacity.max(1);
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + cap {
            self.offset = self.selected + 1 - cap;
        }
        let max = self.entries.len().saturating_sub(cap);
        if self.offset > max {
            self.offset = max;
        }
    }

    pub fn tile_rect(&self, area: Rect, idx: usize) -> Option<Rect> {
        let pos = idx.checked_sub(self.offset)?;
        (pos < self.tiles.capacity(area)).then(|| self.tiles.rect(area, pos))
    }

    pub fn hit(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        let idx = self.offset + self.tiles.at(area, x, y)?;
        (idx < self.entries.len()).then_some(idx)
    }

    pub fn select(&mut self, idx: usize, _area: Rect) {
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = idx.min(self.entries.len() - 1);
        self.clamp_offset();
    }

    fn step(&mut self, dcol: isize, drow: isize, area: Rect) {
        if self.entries.is_empty() {
            return;
        }
        let cols = self.tiles.cols as isize;
        let last = self.entries.len() as isize - 1;
        let next = (self.selected as isize + dcol + drow * cols).clamp(0, last);
        self.select(next as usize, area);
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.entries.get(self.selected).map(|e| e.path.clone())
    }

    // ------------------------------------------------------------------ menu

    /// The rows the menu would have. Only a picture gets "Set as wallpaper".
    pub fn menu_items(&self) -> Vec<MenuItem> {
        let mut items = Vec::with_capacity(4);
        let on_icon = self.menu.as_ref().map_or(false, |m| m.on_icon);
        let entry = if on_icon {
            self.entries.get(self.selected)
        } else {
            None
        };
        if let Some(e) = entry {
            items.push(MenuItem::Open);
            if !e.is_dir && crate::files::is_image(&e.name) {
                items.push(MenuItem::SetWallpaper);
            }
        }
        items.push(MenuItem::Refresh);
        items.push(MenuItem::Leave);
        items
    }

    pub fn open_menu(&mut self, x: u16, y: u16, on_icon: bool) {
        self.menu = Some(Menu {
            x,
            y,
            item: 0,
            on_icon,
        });
    }

    pub fn menu_rect(&self, screen: Rect) -> Option<Rect> {
        let menu = self.menu.as_ref()?;
        let labels: Vec<&str> = self.menu_items().iter().map(|i| i.label()).collect();
        Some(crate::files::popup_rect((menu.x, menu.y), &labels, screen))
    }

    pub fn menu_item_at(&self, screen: Rect, x: u16, y: u16) -> Option<usize> {
        let rect = self.menu_rect(screen)?;
        crate::files::popup_item_at(rect, self.menu_items().len(), x, y)
    }

    fn menu_item(&mut self, idx: usize) {
        if let Some(m) = self.menu.as_mut() {
            m.item = idx;
        }
    }

    fn menu_move(&mut self, delta: isize) {
        let last = self.menu_items().len() as isize - 1;
        if let Some(m) = self.menu.as_mut() {
            m.item = ((m.item as isize + delta).clamp(0, last)) as usize;
        }
    }

    fn apply_menu_item(&mut self, item: MenuItem) -> DesktopAction {
        self.menu = None;
        match item {
            MenuItem::Open => self
                .selected_path()
                .map(DesktopAction::Open)
                .unwrap_or(DesktopAction::None),
            MenuItem::SetWallpaper => match self.entries.get(self.selected).filter(|e| !e.is_dir) {
                Some(e) => DesktopAction::SetWallpaper(e.path.clone()),
                None => DesktopAction::None,
            },
            MenuItem::Refresh => {
                self.reload();
                self.status = "desktop reloaded".into();
                DesktopAction::None
            }
            MenuItem::Leave => DesktopAction::Hide,
        }
    }

    // ---------------------------------------------------------------- input

    pub fn on_key(&mut self, ev: KeyEvent, area: Rect) -> DesktopAction {
        // A menu is modal: Escape closes it rather than the desktop, or the
        // first Escape after a right click takes everything away at once.
        if self.menu.is_some() {
            match ev.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.menu = None;
                    DesktopAction::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.menu_move(-1);
                    DesktopAction::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.menu_move(1);
                    DesktopAction::None
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    self.menu_item(0);
                    DesktopAction::None
                }
                KeyCode::End | KeyCode::Char('G') => {
                    let last = self.menu_items().len().saturating_sub(1);
                    self.menu_item(last);
                    DesktopAction::None
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let idx = self.menu.as_ref().map(|m| m.item).unwrap_or(0);
                    match self.menu_items().get(idx).copied() {
                        Some(item) => self.apply_menu_item(item),
                        None => DesktopAction::None,
                    }
                }
                _ => DesktopAction::None,
            }
        } else {
            self.on_desktop_key(ev, area)
        }
    }

    fn on_desktop_key(&mut self, ev: KeyEvent, area: Rect) -> DesktopAction {
        let step = self.tiles.cols as isize;
        match ev.code {
            KeyCode::Esc | KeyCode::Char('q') => return DesktopAction::Hide,
            KeyCode::Left | KeyCode::Char('h') => self.step(-1, 0, area),
            KeyCode::Right | KeyCode::Char('l') => self.step(1, 0, area),
            KeyCode::Up | KeyCode::Char('k') => self.step(0, -1, area),
            KeyCode::Down | KeyCode::Char('j') => self.step(0, 1, area),
            KeyCode::Tab => self.step(step, 0, area),
            KeyCode::Enter | KeyCode::Char(' ') => {
                return match self.selected_path() {
                    Some(p) => DesktopAction::Open(p),
                    None => DesktopAction::None,
                }
            }
            KeyCode::Char('r') => {
                self.reload();
                self.status = "desktop reloaded".into();
            }
            _ => {}
        }
        DesktopAction::None
    }

    pub fn on_mouse(&mut self, ev: MouseEvent, area: Rect) -> DesktopAction {
        if self.menu.is_some() {
            match ev.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(idx) = self.menu_item_at(area, ev.column, ev.row) {
                        let item = self.menu_items()[idx];
                        return self.apply_menu_item(item);
                    }
                    self.menu = None;
                    return DesktopAction::None;
                }
                // A right click somewhere else moves the menu there instead of
                // being swallowed by the one already up.
                MouseEventKind::Down(MouseButton::Right) => self.menu = None,
                _ => return DesktopAction::None,
            }
        }
        match ev.kind {
            MouseEventKind::ScrollUp => {
                self.step(0, -1, area);
                DesktopAction::None
            }
            MouseEventKind::ScrollDown => {
                self.step(0, 1, area);
                DesktopAction::None
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Right-click selects what it points at, then opens the menu —
                // the same order as every other desktop, so the menu's subject
                // is visible behind it.
                let hit = self.hit(area, ev.column, ev.row);
                if let Some(idx) = hit {
                    self.select(idx, area);
                }
                self.last_click = None;
                self.open_menu(ev.column, ev.row, hit.is_some());
                DesktopAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(idx) = self.hit(area, ev.column, ev.row) else {
                    return DesktopAction::None;
                };
                let double = self
                    .last_click
                    .map(|(t, i)| i == idx && t.elapsed() < DOUBLE_CLICK)
                    .unwrap_or(false);
                self.select(idx, area);
                if double {
                    self.last_click = None;
                    return self
                        .selected_path()
                        .map(DesktopAction::Open)
                        .unwrap_or(DesktopAction::None);
                }
                self.last_click = Some((Instant::now(), idx));
                DesktopAction::None
            }
            _ => DesktopAction::None,
        }
    }
}

/// Where the desktop folder is. `~/.config/user-dirs.dirs` is the answer XDG
/// already settled on — it can be localised, or pointed at a Pictures folder —
/// and it costs one line of parsing to respect it.
fn default_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    if let Some(d) = std::env::var_os("XDG_DESKTOP_DIR") {
        return PathBuf::from(d);
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(home.join(".config")));
    if let Some(dir) = config {
        if let Ok(text) = fs::read_to_string(dir.join("user-dirs.dirs")) {
            for line in text.lines() {
                let line = line.trim();
                let Some(rest) = line.strip_prefix("XDG_DESKTOP_DIR=") else {
                    continue;
                };
                let value = rest.trim().trim_matches('"');
                if value.is_empty() {
                    continue;
                }
                let value = value.replace("$HOME", &home.to_string_lossy());
                return PathBuf::from(value);
            }
        }
    }
    home.join("Desktop")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "termland-desktop-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).expect("temp dir");
        p
    }

    fn desktop_with(names: &[&str]) -> Desktop {
        let mut d = Desktop::new();
        d.entries = names
            .iter()
            .map(|n| Entry {
                name: (*n).to_string(),
                path: PathBuf::from("/x").join(n),
                is_dir: false,
                size: 0,
            })
            .collect();
        d
    }

    #[test]
    fn showing_the_desktop_is_the_same_question_as_having_nothing_tiled() {
        let mut d = desktop_with(&["a"]);
        assert!(!d.shows(true), "panes are on screen, so they are the session");
        assert!(d.shows(false), "an empty workspace is a desktop");
        d.visible = true;
        assert!(d.shows(true), "asked for explicitly, panes or not");
    }

    #[test]
    fn icons_come_from_the_folder_and_hidden_files_stay_hidden() {
        let dir = tmpdir("read");
        fs::write(dir.join("note.txt"), b"hi").unwrap();
        fs::create_dir_all(dir.join("Photos")).unwrap();
        fs::write(dir.join(".secret"), b"no").unwrap();
        let mut d = Desktop::new();
        d.set_dir(dir.clone());
        let names: Vec<&str> = d.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Photos", "note.txt"], "folders first, no dotfiles");
        assert!(d.entries[0].is_dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_click_selects_and_a_second_one_opens() {
        let area = Rect::new(0, 0, 60, 24);
        let mut d = desktop_with(&["one", "two", "three"]);
        d.set_viewport(area);
        let r = d.tile_rect(area, 1).expect("second tile is visible");
        let at = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: r.x + 1,
            row: r.y + 1,
            modifiers: ratatui::crossterm::event::KeyModifiers::empty(),
        };
        assert_eq!(d.on_mouse(at, area), DesktopAction::None);
        assert_eq!(d.selected, 1);
        // The second click on the same icon is an open, and it names that icon.
        match d.on_mouse(at, area) {
            DesktopAction::Open(p) => assert_eq!(p, PathBuf::from("/x/two")),
            other => panic!("expected an open, got {other:?}"),
        }
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: ratatui::crossterm::event::KeyModifiers::empty(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::empty())
    }

    #[test]
    fn the_right_button_offers_the_desktop_its_own_menu() {
        let area = Rect::new(0, 0, 60, 24);
        let mut d = desktop_with(&["sky.png", "notes.txt"]);
        d.set_viewport(area);

        // On an icon: it selects that icon and offers to open it.
        let r = d.tile_rect(area, 0).expect("visible");
        assert_eq!(
            d.on_mouse(mouse(MouseEventKind::Down(MouseButton::Right), r.x + 1, r.y + 1), area),
            DesktopAction::None
        );
        assert_eq!(d.selected, 0, "the menu's subject is the icon under it");
        assert_eq!(
            d.menu_items(),
            vec![
                MenuItem::Open,
                MenuItem::SetWallpaper,
                MenuItem::Refresh,
                MenuItem::Leave
            ]
        );

        // On the bare wallpaper: no icon to speak of, so no "Open".
        d.on_mouse(mouse(MouseEventKind::Down(MouseButton::Right), 59, 23), area);
        assert_eq!(d.menu_items(), vec![MenuItem::Refresh, MenuItem::Leave]);

        // A text file is not a wallpaper either.
        let r = d.tile_rect(area, 1).expect("visible");
        d.on_mouse(mouse(MouseEventKind::Down(MouseButton::Right), r.x + 1, r.y + 1), area);
        assert_eq!(d.menu_items(), vec![MenuItem::Open, MenuItem::Refresh, MenuItem::Leave]);
    }

    #[test]
    fn choosing_set_as_wallpaper_names_the_picture_under_the_pointer() {
        let area = Rect::new(0, 0, 60, 24);
        let mut d = desktop_with(&["sky.png"]);
        d.set_viewport(area);
        let r = d.tile_rect(area, 0).unwrap();
        d.on_mouse(mouse(MouseEventKind::Down(MouseButton::Right), r.x + 1, r.y + 1), area);

        // The row, clicked where the menu put it.
        let rect = d.menu_rect(area).expect("a menu is up");
        let row = d
            .menu_items()
            .iter()
            .position(|i| *i == MenuItem::SetWallpaper)
            .expect("the row is there");
        match d.on_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 2, rect.y + 1 + row as u16),
            area,
        ) {
            DesktopAction::SetWallpaper(p) => assert_eq!(p, PathBuf::from("/x/sky.png")),
            other => panic!("expected a wallpaper, got {other:?}"),
        }
        assert!(d.menu.is_none(), "a chosen row closes the menu");
    }

    #[test]
    fn escape_closes_the_menu_before_it_closes_the_desktop() {
        let area = Rect::new(0, 0, 60, 24);
        let mut d = desktop_with(&["a"]);
        d.set_viewport(area);
        d.visible = true;
        d.open_menu(3, 3, true);
        assert_eq!(d.on_key(key(KeyCode::Esc), area), DesktopAction::None);
        assert!(d.menu.is_none());
        assert_eq!(
            d.on_key(key(KeyCode::Esc), area),
            DesktopAction::Hide,
            "the next Escape is the desktop's"
        );
    }

    #[test]
    fn the_menu_takes_the_keyboard_and_leave_really_leaves() {
        let area = Rect::new(0, 0, 60, 24);
        let mut d = desktop_with(&["a"]);
        d.set_viewport(area);
        d.open_menu(2, 2, false);
        d.on_key(key(KeyCode::End), area);
        assert_eq!(d.menu.as_ref().map(|m| m.item), Some(1), "the last row");
        assert_eq!(d.on_key(key(KeyCode::Enter), area), DesktopAction::Hide);
        assert!(d.menu.is_none());
    }

    #[test]
    fn the_arrows_walk_the_grid_a_row_at_a_time() {
        let area = Rect::new(0, 0, 60, 24);
        let mut d = desktop_with(&["a", "b", "c", "d", "e", "f", "g"]);
        d.set_viewport(area);
        let cols = d.tiles.cols;
        let key = |code| KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::empty());
        d.on_key(key(KeyCode::Right), area);
        assert_eq!(d.selected, 1);
        d.on_key(key(KeyCode::Down), area);
        assert_eq!(d.selected, 1 + cols, "down is a whole row of icons");
        d.on_key(key(KeyCode::Up), area);
        assert_eq!(d.selected, 1);
    }

    #[test]
    fn enter_opens_the_selection_and_escape_leaves_desktop_mode() {
        let area = Rect::new(0, 0, 60, 24);
        let mut d = desktop_with(&["a", "b"]);
        d.set_viewport(area);
        let key = |code| KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::empty());
        match d.on_key(key(KeyCode::Enter), area) {
            DesktopAction::Open(p) => assert_eq!(p, PathBuf::from("/x/a")),
            other => panic!("expected an open, got {other:?}"),
        }
        assert_eq!(d.on_key(key(KeyCode::Esc), area), DesktopAction::Hide);
    }

    #[test]
    fn an_empty_desktop_is_not_a_panic() {
        let area = Rect::new(0, 0, 40, 12);
        let mut d = desktop_with(&[]);
        d.set_viewport(area);
        let key = |code| KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::empty());
        assert_eq!(d.on_key(key(KeyCode::Down), area), DesktopAction::None);
        assert_eq!(d.on_key(key(KeyCode::Enter), area), DesktopAction::None);
        assert_eq!(d.selected, 0);
        assert_eq!(d.hit(area, 1, 1), None);
    }

    #[test]
    fn the_selection_scrolls_into_view_rather_than_staying_put() {
        let area = Rect::new(0, 0, 60, 16);
        let names: Vec<String> = (0..40).map(|i| format!("f{i}")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let mut d = desktop_with(&refs);
        d.set_viewport(area);
        let cap = d.tiles.capacity(area);
        d.select(39, area);
        assert!(d.offset + cap > 39);
        assert!(d.tile_rect(area, 39).is_some());
    }
}
