//! The built-in file manager pane.
//!
//! A tiling OS front end cannot make its file browsing someone else's program:
//! it is the one pane with right-click, drag-free selection and a context menu,
//! and it has to look like the rest of the desktop. So this is a real one, not
//! a launcher for yazi/ranger.
//!
//! The pane owns no drawing code and no terminal: it reports what the user did
//! (`FileAction`) and the app decides where a file opens. That keeps the state
//! machine testable on the JVM with no tty and no compositor.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How close two clicks must be to count as a double-click. Dolphin's default
/// is 400 ms; 450 is forgiving on a trackpad without feeling sticky.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// A row of the context menu.
///
/// The rows are built from the selection instead of being a fixed list, because
/// one of them only makes sense for some files: "Set as wallpaper" on a shell
/// script is a menu item that lies about what a click will do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Open,
    OpenInPane,
    SetWallpaper,
    Rename,
    NewFolder,
    Delete,
    Refresh,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Open => "Open",
            MenuItem::OpenInPane => "Open in new pane",
            MenuItem::SetWallpaper => "Set as wallpaper",
            MenuItem::Rename => "Rename…",
            MenuItem::NewFolder => "New folder…",
            MenuItem::Delete => "Delete",
            MenuItem::Refresh => "Refresh",
        }
    }
}

/// The rows for `entry` — the file or folder the menu would act on.
///
/// With nothing selected (an empty folder) only the rows that do not need a
/// file survive: make a folder, reread the listing. A `..` row is a way out of
/// the folder, not a file: it cannot be renamed, deleted, or become a wallpaper.
pub fn menu_items(entry: Option<&Entry>) -> Vec<MenuItem> {
    let real = entry.filter(|e| !e.is_parent());
    let mut items = Vec::with_capacity(7);
    if let Some(e) = real {
        items.push(MenuItem::Open);
        items.push(MenuItem::OpenInPane);
        if !e.is_dir && is_image(&e.name) {
            items.push(MenuItem::SetWallpaper);
        }
        items.push(MenuItem::Rename);
    }
    items.push(MenuItem::NewFolder);
    if real.is_some() {
        items.push(MenuItem::Delete);
    }
    items.push(MenuItem::Refresh);
    items
}

/// Whether this file is a picture, by the same test the icon uses to draw one:
/// a file the grid paints as a photo is a file that can be a wallpaper.
pub fn is_image(name: &str) -> bool {
    crate::icons::Kind::of(name, false, false) == crate::icons::Kind::Image
}

/// How the pane draws its entries. Dolphin has three views; these are the two
/// that earn their keep here: the list, for reading names and sizes, and the
/// grid, for finding a thing by looking at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileView {
    List,
    Grid,
}

impl FileView {
    pub fn name(self) -> &'static str {
        match self {
            FileView::List => "list",
            FileView::Grid => "icons",
        }
    }
}

/// The grid's geometry. Computed once from the pane's body and then used by the
/// renderer, the hit test and the keyboard, so a tile that is drawn somewhere
/// cannot be clicked somewhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tiles {
    pub cols: usize,
    /// Tile width in cells.
    pub w: u16,
    pub h: u16,
    /// Leftover cells split between the two edges. Centring them is what keeps
    /// the last column a whole tile: sharing the remainder *between* columns
    /// made the rightmost one narrow, and a narrow tile clips its own name.
    pub pad: u16,
}

impl Tiles {
    /// Five rows of icon and two of name, and no gap: the name is part of the
    /// tile, which is what makes it clickable as one thing.
    pub const H: u16 = 7;
    pub const MIN_W: u16 = 8;
    pub const PREF_W: u16 = 12;

    pub fn for_area(area: Rect) -> Self {
        Self::for_width(area, Self::PREF_W)
    }

    /// Same grid with a wider tile. The desktop wants fewer, roomier icons than
    /// a file pane: names there are read from a distance, not scanned in a list.
    pub fn for_width(area: Rect, prefer: u16) -> Self {
        let width = area.width.max(1);
        let mut cols = (width / prefer.max(Self::MIN_W)).max(1) as usize;
        // Never fewer than MIN_W per tile: at 8 cells a name is unreadable, so
        // a narrow pane shows one column rather than five slivers.
        while cols > 1 && (width / cols as u16) < Self::MIN_W {
            cols -= 1;
        }
        let w = (width / cols as u16).max(1);
        Self {
            cols,
            w,
            h: Self::H,
            pad: width.saturating_sub(cols as u16 * w) / 2,
        }
    }

    /// Rows of tiles that fit.
    pub fn rows(&self, area: Rect) -> usize {
        (area.height / self.h).max(1) as usize
    }

    /// How many tiles the body can show at once.
    pub fn capacity(&self, area: Rect) -> usize {
        self.cols * self.rows(area)
    }

    /// Position of the tile at `pos` of the visible grid.
    pub fn rect(&self, area: Rect, pos: usize) -> Rect {
        let col = (pos % self.cols) as u16;
        let row = (pos / self.cols) as u16;
        Rect {
            x: area.x + self.pad + col * self.w,
            y: area.y + row * self.h,
            width: self.w.min(area.width.saturating_sub(col * self.w)),
            height: self.h.min(area.height.saturating_sub(row * self.h)),
        }
    }

    /// The visible position under a cell, if it is inside the body.
    pub fn at(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        let left = area.x + self.pad;
        if x < left || y < area.y || x >= left + self.cols as u16 * self.w || y >= area.y + area.height {
            return None;
        }
        let col = ((x - left) / self.w) as usize;
        let row = ((y - area.y) / self.h) as usize;
        if col >= self.cols || row >= self.rows(area) {
            return None;
        }
        Some(row * self.cols + col)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

impl Entry {
    pub fn is_parent(&self) -> bool {
        self.name == ".."
    }
}

/// What the user asked for. `Open` names a file (or directory) the app should
/// open somewhere; `OpenInPane` asks for a *new* pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileAction {
    None,
    Open(PathBuf, bool),
    OpenInPane(PathBuf, bool),
    /// Ask for the context menu on the selection, anchored by the app because
    /// only the app knows where the pane is on screen.
    ContextMenu,
    /// Use this picture as the desktop wallpaper. The desktop is not this
    /// pane's business, so the request travels as an action.
    SetWallpaper(PathBuf),
    ClosePane,
}

/// Which modal is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKind {
    NewFolder,
    Rename(PathBuf),
    /// Path plus whether it is a directory, which decides the removal call.
    Delete(PathBuf, bool),
}

#[derive(Clone, Debug)]
pub struct Dialog {
    pub kind: DialogKind,
    pub input: String,
    pub error: String,
}

impl Dialog {
    pub fn prompt(&self) -> String {
        match &self.kind {
            DialogKind::NewFolder => "New folder name".into(),
            DialogKind::Rename(_) => "Rename to".into(),
            DialogKind::Delete(p, _) => {
                format!("Delete {}?", display_name(p))
            }
        }
    }

    pub fn is_confirm(&self) -> bool {
        matches!(self.kind, DialogKind::Delete(..))
    }
}

#[derive(Clone, Debug)]
pub struct Menu {
    /// Anchor, in screen cells: where the pointer was.
    pub x: u16,
    pub y: u16,
    pub item: usize,
}

pub struct FilePane {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// First visible row, kept so the selection never leaves the viewport.
    pub offset: usize,
    pub show_hidden: bool,
    pub view: FileView,
    /// Grid geometry from the last frame; `rows` is derived from it in grid
    /// mode so every existing "keep the selection visible" path keeps working.
    pub tiles: Tiles,
    pub status: String,
    pub menu: Option<Menu>,
    pub dialog: Option<Dialog>,
    /// Viewport height in rows, set by the renderer each frame.
    pub rows: u16,
    last_click: Option<(Instant, usize)>,
}

impl FilePane {
    /// What the pane calls itself in its header: the folder's own name, not
    /// its path. A header has room for about twenty cells before it is cut, so
    /// `/home/yassine/Pictures/hyprland-showcase` reaches the screen as
    /// `/home/yassine/Pictu` — a path nobody can read. The home directory is
    /// `~`, and a directory with no name of its own (the root) keeps its path.
    pub fn title(&self) -> String {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            if self.cwd == home {
                return "~".to_string();
            }
        }
        match self.cwd.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.cwd.display().to_string(),
        }
    }

    pub fn new(dir: Option<PathBuf>) -> Self {
        let cwd = dir
            .filter(|p| p.is_dir())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut fp = Self {
            cwd: cwd.clone(),
            entries: Vec::new(),
            selected: 0,
            offset: 0,
            show_hidden: false,
            view: FileView::List,
            tiles: Tiles::for_area(Rect::new(0, 0, 72, 24)),
            status: String::new(),
            menu: None,
            dialog: None,
            rows: 20,
            last_click: None,
        };
        fp.set_dir(cwd);
        fp
    }

    // ------------------------------------------------------------- directory

    pub fn set_dir(&mut self, dir: PathBuf) {
        match fs::canonicalize(&dir) {
            Ok(p) if p.is_dir() => self.cwd = p,
            _ => {
                self.status = format!("cannot open {}", dir.display());
                return;
            }
        }
        self.selected = 0;
        self.offset = 0;
        self.menu = None;
        self.dialog = None;
        self.reload();
    }

    /// Re-read the directory, keeping the selection on the same name when it
    /// still exists. Deleting a file should not move the cursor to the top.
    pub fn reload(&mut self) {
        let keep = self.entries.get(self.selected).map(|e| e.name.clone());
        let mut entries = Vec::new();
        if let Some(parent) = self.cwd.parent() {
            entries.push(Entry {
                name: "..".into(),
                path: parent.to_path_buf(),
                is_dir: true,
                size: 0,
            });
        }
        match fs::read_dir(&self.cwd) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let path = e.path();
                    let meta = e.metadata().ok();
                    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                    // A dangling symlink is a file that cannot be opened; keep
                    // it visible (it is real) but it will report its own error.
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    entries.push(Entry {
                        name,
                        path,
                        is_dir,
                        size,
                    });
                }
            }
            Err(e) => {
                self.status = format!("{}: {e}", self.cwd.display());
            }
        }
        sort_entries(&mut entries);
        self.entries = entries;
        self.selected = keep
            .and_then(|name| self.entries.iter().position(|e| e.name == name))
            .unwrap_or(0)
            .min(self.entries.len().saturating_sub(1));
        self.clamp_offset();
        self.status.clear();
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.reload();
        self.status = if self.show_hidden {
            "hidden files shown".into()
        } else {
            "hidden files hidden".into()
        };
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    // ------------------------------------------------------------- selection

    pub fn set_rows(&mut self, rows: u16) {
        self.rows = rows.max(1);
        self.clamp_offset();
    }

    /// The body changed size, or the view changed: re-derive the viewport. In
    /// grid mode a "row" is one tile, so `rows` is the whole grid's capacity
    /// and every offset/scroll path below works unchanged.
    pub fn set_viewport(&mut self, area: Rect) {
        self.tiles = Tiles::for_area(area);
        self.rows = match self.view {
            FileView::Grid => self.tiles.capacity(area) as u16,
            FileView::List => area.height.max(1),
        };
        self.clamp_offset();
    }

    pub fn set_view(&mut self, view: FileView) {
        if self.view == view {
            return;
        }
        self.view = view;
        self.offset = 0;
        self.status = format!("{} view", view.name());
    }

    pub fn toggle_view(&mut self) {
        self.set_view(match self.view {
            FileView::List => FileView::Grid,
            FileView::Grid => FileView::List,
        });
    }

    /// The tile a visible entry index occupies, for drawing and hover.
    pub fn tile_rect(&self, area: Rect, idx: usize) -> Option<Rect> {
        let pos = idx.checked_sub(self.offset)?;
        (self.view == FileView::Grid && pos < self.tiles.capacity(area))
            .then(|| self.tiles.rect(area, pos))
    }

    /// Entry index under a cell of the body. The one place that knows a click
    /// means row or tile, so the mouse cannot disagree with what is drawn.
    pub fn hit(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        let idx = match self.view {
            FileView::List => self.index_at(area, y)?,
            FileView::Grid => self.offset + self.tiles.at(area, x, y)?,
        };
        (idx < self.entries.len()).then_some(idx)
    }

    /// Arrow keys: a step along the grid, keeping the column when moving by a
    /// row, which is what makes a grid navigable.
    fn step(&mut self, dcol: isize, drow: isize) {
        if self.entries.is_empty() {
            return;
        }
        let cols = self.tiles.cols as isize;
        let last = self.entries.len() as isize - 1;
        let next = (self.selected as isize + dcol + drow * cols).clamp(0, last);
        self.selected = next as usize;
        self.clamp_offset();
    }

    pub fn select(&mut self, idx: usize) {
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = idx.min(self.entries.len() - 1);
        self.clamp_offset();
    }

    pub fn move_sel(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last);
        self.selected = next as usize;
        self.clamp_offset();
    }

    pub fn page(&mut self, pages: isize) {
        let step = self.rows.saturating_sub(1).max(1) as isize;
        self.move_sel(pages * step);
    }

    pub fn clamp_offset(&mut self) {
        let rows = self.rows.max(1) as usize;
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + rows {
            self.offset = self.selected + 1 - rows;
        }
        let max = self.entries.len().saturating_sub(rows);
        self.offset = self.offset.min(max);
    }

    /// Row in the viewport for entry `idx`, if it is visible.
    pub fn row_of(&self, idx: usize) -> Option<u16> {
        let row = idx.checked_sub(self.offset)?;
        if row < self.rows as usize {
            Some(row as u16)
        } else {
            None
        }
    }

    /// Entry index for a click at `y` inside `content`.
    pub fn index_at(&self, content: Rect, y: u16) -> Option<usize> {
        if y < content.y {
            return None;
        }
        let row = (y - content.y) as usize;
        if row >= self.rows as usize {
            return None;
        }
        let idx = self.offset + row;
        if idx < self.entries.len() {
            Some(idx)
        } else {
            None
        }
    }

    // ---------------------------------------------------------------- opening

    /// Enter a directory, or report a file for the app to open.
    fn activate(&mut self) -> FileAction {
        let Some(e) = self.selected_entry().cloned() else {
            return FileAction::None;
        };
        if e.is_dir {
            self.set_dir(e.path);
            FileAction::None
        } else {
            FileAction::Open(e.path, false)
        }
    }

    fn open_in_pane(&mut self) -> FileAction {
        let Some(e) = self.selected_entry().cloned() else {
            return FileAction::None;
        };
        if e.is_parent() {
            return FileAction::None;
        }
        FileAction::OpenInPane(e.path, e.is_dir)
    }

    // --------------------------------------------------------------- dialogs

    fn start_dialog(&mut self, kind: DialogKind) {
        let input = match &kind {
            DialogKind::Rename(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.menu = None;
        self.dialog = Some(Dialog {
            kind,
            input,
            error: String::new(),
        });
    }

    /// Go through the open dialog. Returns a message only when something
    /// actually happened, so a failed rename can leave the dialog up with an
    /// error instead of vanishing.
    fn apply_dialog(&mut self) -> Option<String> {
        let Some(dialog) = self.dialog.take() else {
            return None;
        };
        let result = match &dialog.kind {
            DialogKind::NewFolder => {
                let name = dialog.input.trim().to_string();
                if name.is_empty() {
                    Err("a folder needs a name".to_string())
                } else if self.cwd.join(&name).exists() {
                    Err(format!("{name} already exists"))
                } else {
                    fs::create_dir(self.cwd.join(&name))
                        .map(|_| format!("created {name}"))
                        .map_err(|e| format!("{name}: {e}"))
                }
            }
            DialogKind::Rename(from) => {
                let name = dialog.input.trim().to_string();
                if name.is_empty() {
                    Err("a name is required".to_string())
                } else {
                    let to = from
                        .parent()
                        .map(|p| p.join(&name))
                        .unwrap_or_else(|| PathBuf::from(&name));
                    if to.exists() {
                        Err(format!("{name} already exists"))
                    } else {
                        fs::rename(from, &to)
                            .map(|_| format!("renamed to {name}"))
                            .map_err(|e| format!("rename: {e}"))
                    }
                }
            }
            DialogKind::Delete(path, is_dir) => {
                let res = if *is_dir {
                    fs::remove_dir_all(path)
                } else {
                    fs::remove_file(path)
                };
                res.map(|_| format!("deleted {}", display_name(path)))
                    .map_err(|e| format!("delete: {e}"))
            }
        };
        match result {
            Ok(msg) => {
                // A rename changes the name, so the old selection cannot be
                // used to find the row again: the file it named is gone.
                let renamed = match &dialog.kind {
                    DialogKind::Rename(_) => Some(dialog.input.trim().to_string()),
                    _ => None,
                };
                self.reload();
                if let Some(name) = renamed {
                    if let Some(i) = self.entries.iter().position(|e| e.name == name) {
                        self.selected = i;
                        self.clamp_offset();
                    }
                }
                Some(msg)
            }
            Err(e) => {
                self.dialog = Some(Dialog {
                    error: e.clone(),
                    ..dialog
                });
                Some(e)
            }
        }
    }

    fn dialog_key(&mut self, ev: KeyEvent) -> FileAction {
        let Some(dialog) = self.dialog.as_mut() else {
            return FileAction::None;
        };
        if dialog.is_confirm() {
            match ev.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.apply_dialog();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.dialog = None;
                }
                _ => {}
            }
            return FileAction::None;
        }
        match ev.code {
            KeyCode::Esc => {
                self.dialog = None;
            }
            KeyCode::Enter => {
                self.apply_dialog();
            }
            KeyCode::Backspace => {
                dialog.input.pop();
            }
            KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                // A file name may contain anything but `/` and NUL; blocking
                // the separator here is cheaper than a failed rename later.
                if c != '/' {
                    dialog.input.push(c);
                }
            }
            _ => {}
        }
        FileAction::None
    }

    // ----------------------------------------------------------------- menu

    pub fn open_menu(&mut self, x: u16, y: u16) {
        self.menu = Some(Menu { x, y, item: 0 });
    }

    /// The rows this pane's menu has right now. The selection cannot change
    /// while the menu is up (it is modal), so asking again is the same answer.
    pub fn menu_items(&self) -> Vec<MenuItem> {
        menu_items(self.selected_entry())
    }

    fn menu_move(&mut self, delta: isize) {
        let last = self.menu_items().len() as isize - 1;
        if let Some(m) = self.menu.as_mut() {
            m.item = ((m.item as isize + delta).clamp(0, last)) as usize;
        }
    }

    fn apply_menu_item(&mut self, item: MenuItem) -> FileAction {
        self.menu = None;
        match item {
            MenuItem::Open => self.activate(),
            MenuItem::OpenInPane => self.open_in_pane(),
            MenuItem::SetWallpaper => {
                match self.selected_entry().filter(|e| !e.is_dir).cloned() {
                    Some(e) => FileAction::SetWallpaper(e.path),
                    None => FileAction::None,
                }
            }
            MenuItem::Rename => {
                if let Some(e) = self.selected_entry().filter(|e| !e.is_parent()).cloned() {
                    self.start_dialog(DialogKind::Rename(e.path));
                }
                FileAction::None
            }
            MenuItem::NewFolder => {
                self.start_dialog(DialogKind::NewFolder);
                FileAction::None
            }
            MenuItem::Delete => {
                if let Some(e) = self.selected_entry().filter(|e| !e.is_parent()).cloned() {
                    self.start_dialog(DialogKind::Delete(e.path, e.is_dir));
                }
                FileAction::None
            }
            MenuItem::Refresh => {
                self.reload();
                FileAction::None
            }
        }
    }

    // ----------------------------------------------------------------- input

    pub fn on_key(&mut self, ev: KeyEvent) -> FileAction {
        if self.dialog.is_some() {
            return self.dialog_key(ev);
        }
        if self.menu.is_some() {
            match ev.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.menu = None;
                    return FileAction::None;
                }
                KeyCode::Up | KeyCode::Char('k') => self.menu_move(-1),
                KeyCode::Down | KeyCode::Char('j') => self.menu_move(1),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let idx = self.menu.as_ref().map(|m| m.item).unwrap_or(0);
                    let item = self.menu_items().get(idx).copied();
                    return match item {
                        Some(item) => self.apply_menu_item(item),
                        None => FileAction::None,
                    };
                }
                _ => {}
            }
            return FileAction::None;
        }

        match ev.code {
            KeyCode::Esc => FileAction::None,
            KeyCode::Char('q') => FileAction::ClosePane,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_sel(if self.view == FileView::Grid {
                    -(self.tiles.cols as isize)
                } else {
                    -1
                });
                FileAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_sel(if self.view == FileView::Grid {
                    self.tiles.cols as isize
                } else {
                    1
                });
                FileAction::None
            }
            // Arrows navigate in the grid; h/l stay "up a level" and "open", so
            // the muscle memory from the list view survives the switch.
            KeyCode::Left if self.view == FileView::Grid => {
                self.step(-1, 0);
                FileAction::None
            }
            KeyCode::Right if self.view == FileView::Grid => {
                self.step(1, 0);
                FileAction::None
            }
            KeyCode::Char('v') => {
                self.toggle_view();
                FileAction::None
            }
            KeyCode::PageUp => {
                self.page(-1);
                FileAction::None
            }
            KeyCode::PageDown => {
                self.page(1);
                FileAction::None
            }
            KeyCode::Home => {
                self.select(0);
                FileAction::None
            }
            KeyCode::End => {
                self.select(self.entries.len().saturating_sub(1));
                FileAction::None
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.activate(),
            KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
                if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
                    self.set_dir(parent);
                }
                FileAction::None
            }
            KeyCode::Char('r') => {
                if let Some(e) = self.selected_entry().filter(|e| !e.is_parent()).cloned() {
                    self.start_dialog(DialogKind::Rename(e.path));
                }
                FileAction::None
            }
            KeyCode::Char('n') => {
                self.start_dialog(DialogKind::NewFolder);
                FileAction::None
            }
            // The universal "context menu" keys: Shift+F10, and the Menu key
            // on keyboards that have one. A right-click is not the only way
            // to reach a menu.
            KeyCode::F(10) if ev.modifiers.contains(KeyModifiers::SHIFT) => FileAction::ContextMenu,
            KeyCode::Menu => FileAction::ContextMenu,
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(e) = self.selected_entry().filter(|e| !e.is_parent()).cloned() {
                    self.start_dialog(DialogKind::Delete(e.path, e.is_dir));
                }
                FileAction::None
            }
            KeyCode::Char('.') => {
                self.toggle_hidden();
                FileAction::None
            }
            KeyCode::Char('g') => {
                self.select(0);
                FileAction::None
            }
            KeyCode::Char('G') => {
                self.select(self.entries.len().saturating_sub(1));
                FileAction::None
            }
            KeyCode::F(5) => {
                self.reload();
                FileAction::None
            }
            _ => FileAction::None,
        }
    }

    /// `content` is the pane's body (below the header); `screen` is the whole
    /// window, used to hit-test the context menu, which is allowed to spill
    /// over the pane it belongs to.
    pub fn on_mouse(&mut self, ev: MouseEvent, content: Rect, screen: Rect) -> FileAction {
        if self.dialog.is_some() {
            return FileAction::None;
        }

        if let Some(menu) = &self.menu {
            if ev.kind == MouseEventKind::Down(MouseButton::Left) {
                let items = self.menu_items();
                if let Some(idx) = menu_item_at(menu, &items, screen, ev.column, ev.row) {
                    let item = items[idx];
                    return self.apply_menu_item(item);
                }
                self.menu = None;
            }
            return FileAction::None;
        }

        match ev.kind {
            MouseEventKind::ScrollUp => {
                // Three *rows* of tiles, not three tiles: the wheel scrolls the
                // view, it does not walk the selection across it.
                let step = 3 * self.tiles.cols as isize;
                self.move_sel(if self.view == FileView::Grid { -step } else { -3 });
                FileAction::None
            }
            MouseEventKind::ScrollDown => {
                let step = 3 * self.tiles.cols as isize;
                self.move_sel(if self.view == FileView::Grid { step } else { 3 });
                FileAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(idx) = self.hit(content, ev.column, ev.row) else {
                    return FileAction::None;
                };
                self.select(idx);
                let double = self
                    .last_click
                    .map(|(t, i)| i == idx && t.elapsed() < DOUBLE_CLICK)
                    .unwrap_or(false);
                self.last_click = Some((Instant::now(), idx));
                if double {
                    self.last_click = None;
                    return self.activate();
                }
                FileAction::None
            }
            MouseEventKind::Down(MouseButton::Right) => {
                let Some(idx) = self.hit(content, ev.column, ev.row) else {
                    return FileAction::None;
                };
                self.select(idx);
                self.open_menu(ev.column, ev.row);
                FileAction::None
            }
            _ => FileAction::None,
        }
    }
}

/// Sort directories first, then by name, case-insensitively, with `..` pinned
/// to the top. The parent row has to sort before directories whose names begin
/// with punctuation, which a plain string sort would not do.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let a_parent = a.is_parent();
        let b_parent = b.is_parent();
        b_parent
            .cmp(&a_parent)
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// The context menu's geometry. Shared by the renderer and the hit test, so a
/// menu that is drawn somewhere cannot be clicked somewhere else.
pub fn menu_rect(menu: &Menu, items: &[MenuItem], screen: Rect) -> Rect {
    let labels: Vec<&str> = items.iter().map(|i| i.label()).collect();
    popup_rect((menu.x, menu.y), &labels, screen)
}

/// Which menu row is under `(x, y)`, if any.
pub fn menu_item_at(menu: &Menu, items: &[MenuItem], screen: Rect, x: u16, y: u16) -> Option<usize> {
    popup_item_at(menu_rect(menu, items, screen), items.len(), x, y)
}

/// The geometry of any pop-up list: a border, one row per label, anchored at a
/// point, and never leaving the screen.
///
/// It lives here, once, because the file manager's menu and the desktop's menu
/// are the same box drawn twice — and a second copy of "which row is under the
/// pointer" is a second chance to get the border arithmetic wrong.
pub fn popup_rect(anchor: (u16, u16), labels: &[&str], screen: Rect) -> Rect {
    let width = (labels.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 4) as u16;
    let height = labels.len() as u16 + 2;
    let max_x = screen.x.saturating_add(screen.width).saturating_sub(width);
    let max_y = screen.y.saturating_add(screen.height).saturating_sub(height);
    Rect {
        x: anchor.0.min(max_x).max(screen.x),
        y: anchor.1.min(max_y).max(screen.y),
        width: width.min(screen.width),
        height: height.min(screen.height),
    }
}

/// Which row of a pop-up at `rect` is under `(x, y)`. The border and everything
/// past the last row answer `None`, so a click there closes the menu instead of
/// running whichever row happens to be nearest.
pub fn popup_item_at(rect: Rect, count: usize, x: u16, y: u16) -> Option<usize> {
    if x <= rect.x || x >= rect.x + rect.width.saturating_sub(1) {
        return None;
    }
    if y <= rect.y || y >= rect.y + rect.height.saturating_sub(1) {
        return None;
    }
    let idx = (y - rect.y - 1) as usize;
    (idx < count).then_some(idx)
}

/// The dialog's geometry, also shared with the renderer.
pub fn dialog_rect(screen: Rect) -> Rect {
    let w = 52u16.min(screen.width.saturating_sub(2).max(20));
    let h = 7u16.min(screen.height.saturating_sub(2).max(5));
    Rect {
        x: screen.x + (screen.width.saturating_sub(w)) / 2,
        y: screen.y + (screen.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// Human-readable size for the listing. Two significant decimals at most, and
/// no unit noise for the common case of a small file.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn tiles_fill_the_width_and_never_get_thinner_than_a_name() {
        let t = Tiles::for_area(Rect::new(0, 0, 60, 20));
        assert_eq!(t.cols, 5);
        assert_eq!(t.w, 12);
        // Seven rows per tile, twice (the 20-row body holds two full rows).
        assert_eq!(t.rows(Rect::new(0, 0, 60, 20)), 2);
        assert_eq!(t.capacity(Rect::new(0, 0, 60, 20)), 10);
        assert!(t.h * t.rows(Rect::new(0, 0, 60, 20)) as u16 <= 20);

        // Leftover cells are shared out, so the grid reaches the right edge.
        let t = Tiles::for_area(Rect::new(0, 0, 71, 10));
        assert!(t.w * t.cols as u16 >= 71 - t.cols as u16);

        // A narrow pane gets one readable column, not a wall of slivers.
        let t = Tiles::for_area(Rect::new(0, 0, 12, 10));
        assert_eq!(t.cols, 1);
        assert!(t.w >= Tiles::MIN_W.min(12));
    }

    #[test]
    fn every_column_is_a_whole_tile_at_any_pane_width() {
        // The remainder goes to the two edges, not into the last column: a
        // narrow last tile clips its own name, and a name that is cut is how
        // someone opens the wrong file.
        for width in 8..240u16 {
            for prefer in [Tiles::PREF_W, 16] {
                let area = Rect::new(0, 0, width, 30);
                let t = Tiles::for_width(area, prefer);
                for col in 0..t.cols {
                    let r = t.rect(area, col);
                    assert_eq!(r.width, t.w, "width {width}, column {col}, prefer {prefer}");
                    assert_eq!(t.at(area, r.x, r.y), Some(col), "hit test at width {width}");
                }
                assert!(t.pad < t.w.max(1), "padding never eats a tile");
            }
        }
    }

    #[test]
    fn a_click_lands_on_the_tile_that_was_drawn() {
        let area = Rect::new(3, 2, 60, 20);
        let t = Tiles::for_area(area);
        for pos in 0..t.capacity(area) {
            let r = t.rect(area, pos);
            // Any cell of the tile maps back to that tile, including the last
            // column and row of it.
            for (x, y) in [
                (r.x, r.y),
                (r.x + r.width - 1, r.y),
                (r.x, r.y + r.height - 1),
                (r.x + r.width / 2, r.y + r.height / 2),
            ] {
                assert_eq!(t.at(area, x, y), Some(pos), "tile {pos} at ({x},{y})");
            }
        }
        // Outside the body is nobody's tile.
        assert_eq!(t.at(area, 0, 0), None);
        assert_eq!(t.at(area, area.x + area.width, area.y), None);
    }

    #[test]
    fn the_grid_moves_by_column_and_by_row() {
        let mut fp = FilePane::new(Some(std::env::temp_dir()));
        fp.entries = (0..12)
            .map(|i| Entry {
                name: format!("f{i}"),
                path: PathBuf::from(format!("/f{i}")),
                is_dir: false,
                size: 0,
            })
            .collect();
        let area = Rect::new(0, 0, 60, 20);
        fp.view = FileView::Grid;
        fp.set_viewport(area);
        fp.select(0);

        fp.step(1, 0);
        assert_eq!(fp.selected, 1);
        fp.step(0, 1);
        assert_eq!(fp.selected, 6, "down is one whole row of tiles");
        fp.step(0, -1);
        assert_eq!(fp.selected, 1, "and back up keeps the column");
        // The ends clamp instead of wrapping into the wrong row.
        fp.select(0);
        fp.step(0, -1);
        assert_eq!(fp.selected, 0);
        fp.select(11);
        fp.step(1, 0);
        assert_eq!(fp.selected, 11);
    }

    #[test]
    fn switching_view_puts_the_selection_back_in_sight() {
        let mut fp = FilePane::new(Some(std::env::temp_dir()));
        fp.entries = (0..40)
            .map(|i| Entry {
                name: format!("f{i}"),
                path: PathBuf::from(format!("/f{i}")),
                is_dir: false,
                size: 0,
            })
            .collect();
        let area = Rect::new(0, 0, 60, 20);
        fp.set_viewport(area);
        fp.select(39);
        fp.set_view(FileView::Grid);
        fp.set_viewport(area);
        // 15 tiles fit, so the last entry is on the last visible page.
        let capacity = fp.tiles.capacity(area);
        assert_eq!(fp.offset, 40 - capacity);
        assert_eq!(fp.selected, 39);
        assert!(fp.tile_rect(area, 39).is_some());
        assert_eq!(fp.tile_rect(area, 0), None, "entry 0 scrolled out");
    }

    #[test]
    fn a_file_pane_is_named_after_its_folder_not_its_path() {
        let fp = FilePane::new(Some(PathBuf::from("/usr/share/doc")));
        assert_eq!(fp.title(), "doc", "the header gets a name, not a path");
        // The root has no name of its own, so it keeps the path it has.
        let root = FilePane::new(Some(PathBuf::from("/")));
        assert_eq!(root.title(), "/");
        // Home reads as `~`, which is how every shell spells it.
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            if home.is_dir() {
                assert_eq!(FilePane::new(Some(home)).title(), "~");
            }
        }
    }

    #[test]
    fn the_wheel_scrolls_three_rows_of_tiles() {
        let mut fp = FilePane::new(Some(std::env::temp_dir()));
        fp.entries = (0..60)
            .map(|i| Entry {
                name: format!("f{i}"),
                path: PathBuf::from(format!("/f{i}")),
                is_dir: false,
                size: 0,
            })
            .collect();
        let area = Rect::new(0, 0, 60, 20);
        fp.view = FileView::Grid;
        fp.set_viewport(area);
        let cols = fp.tiles.cols as usize;
        fp.on_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::empty(),
            },
            area,
            area,
        );
        assert_eq!(fp.selected, 3 * cols);
    }


    // The tests need real directories: rename and delete are filesystem calls,
    // and mocking `std::fs` would test the mock instead of the pane.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "termland-files-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn file(&self, name: &str) {
            fs::write(self.0.join(name), b"x").unwrap();
        }
        fn dir(&self, name: &str) {
            fs::create_dir(self.0.join(name)).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn pane(t: &TempDir) -> FilePane {
        FilePane::new(Some(t.0.clone()))
    }

    fn names(fp: &FilePane) -> Vec<&str> {
        fp.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn directories_sort_first_and_the_parent_stays_on_top() {
        let t = TempDir::new("sort");
        t.file("zeta.txt");
        t.dir("Alpha");
        t.dir("beta");
        t.file(".hidden");
        let fp = pane(&t);
        assert_eq!(names(&fp), vec!["..", "Alpha", "beta", "zeta.txt"]);
    }

    #[test]
    fn hidden_files_appear_only_when_asked() {
        let t = TempDir::new("hidden");
        t.file(".secret");
        t.file("visible");
        let mut fp = pane(&t);
        assert!(!names(&fp).contains(&".secret"));
        fp.toggle_hidden();
        assert!(names(&fp).contains(&".secret"));
    }

    #[test]
    fn selection_is_clamped_to_the_listing() {
        let t = TempDir::new("sel");
        t.file("a");
        t.file("b");
        let mut fp = pane(&t);
        fp.select(99);
        assert_eq!(fp.selected, fp.entries.len() - 1);
        fp.move_sel(-99);
        assert_eq!(fp.selected, 0);
    }

    #[test]
    fn the_offset_follows_a_selection_that_leaves_the_viewport() {
        let t = TempDir::new("offset");
        for i in 0..30 {
            t.file(&format!("f{i:02}"));
        }
        let mut fp = pane(&t);
        fp.set_rows(5);
        fp.select(20);
        assert!(fp.offset <= 20 && 20 < fp.offset + 5);
        fp.select(0);
        assert_eq!(fp.offset, 0);
    }

    #[test]
    fn entering_a_directory_and_going_back() {
        let t = TempDir::new("enter");
        t.dir("sub");
        t.file("sub/inner.txt");
        let mut fp = pane(&t);
        let sub = fp.entries.iter().position(|e| e.name == "sub").unwrap();
        fp.select(sub);
        assert_eq!(fp.activate(), FileAction::None);
        assert!(fp.cwd.ends_with("sub"));
        assert!(names(&fp).contains(&"inner.txt"));

        fp.select(0); // ".."
        assert_eq!(fp.activate(), FileAction::None);
        assert_eq!(fp.cwd, fs::canonicalize(&t.0).unwrap());
    }

    #[test]
    fn a_file_reports_itself_for_opening() {
        let t = TempDir::new("open");
        t.file("notes.txt");
        let mut fp = pane(&t);
        let idx = fp.entries.iter().position(|e| e.name == "notes.txt").unwrap();
        fp.select(idx);
        match fp.activate() {
            FileAction::Open(p, is_dir) => {
                assert!(p.ends_with("notes.txt"));
                assert!(!is_dir);
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn the_new_folder_dialog_creates_a_directory_and_stays_on_typos() {
        let t = TempDir::new("mkdir");
        let mut fp = pane(&t);
        fp.start_dialog(DialogKind::NewFolder);
        // An empty name is refused, and the dialog stays up.
        fp.dialog_key(key(KeyCode::Enter));
        assert!(fp.dialog.is_some());
        for c in "docs".chars() {
            fp.dialog_key(key(KeyCode::Char(c)));
        }
        fp.dialog_key(key(KeyCode::Enter));
        assert!(fp.dialog.is_none());
        assert!(t.0.join("docs").is_dir());

        // A second folder with the same name is an error, not a silent no-op.
        fp.start_dialog(DialogKind::NewFolder);
        for c in "docs".chars() {
            fp.dialog_key(key(KeyCode::Char(c)));
        }
        fp.dialog_key(key(KeyCode::Enter));
        assert!(fp.dialog.is_some());
        assert!(fp.dialog.as_ref().unwrap().error.contains("exists"));
    }

    #[test]
    fn rename_moves_the_file_and_reselects_it() {
        let t = TempDir::new("rename");
        t.file("old.txt");
        let mut fp = pane(&t);
        let idx = fp.entries.iter().position(|e| e.name == "old.txt").unwrap();
        fp.select(idx);
        fp.start_dialog(DialogKind::Rename(t.0.join("old.txt")));
        fp.dialog.as_mut().unwrap().input.clear();
        for c in "new.txt".chars() {
            fp.dialog_key(key(KeyCode::Char(c)));
        }
        fp.dialog_key(key(KeyCode::Enter));
        assert!(!t.0.join("old.txt").exists());
        assert!(t.0.join("new.txt").exists());
        assert_eq!(fp.selected_entry().map(|e| e.name.as_str()), Some("new.txt"));
    }

    #[test]
    fn deleting_needs_a_confirm_and_y_removes_the_file() {
        let t = TempDir::new("delete");
        t.file("junk.txt");
        let mut fp = pane(&t);
        let idx = fp.entries.iter().position(|e| e.name == "junk.txt").unwrap();
        fp.select(idx);
        fp.start_dialog(DialogKind::Delete(t.0.join("junk.txt"), false));
        fp.dialog_key(key(KeyCode::Char('n')));
        assert!(fp.dialog.is_none());
        assert!(t.0.join("junk.txt").exists());

        fp.start_dialog(DialogKind::Delete(t.0.join("junk.txt"), false));
        fp.dialog_key(key(KeyCode::Char('y')));
        assert!(!t.0.join("junk.txt").exists());
    }

    #[test]
    fn a_folder_is_deleted_with_its_contents() {
        let t = TempDir::new("rmdir");
        t.dir("tree");
        t.file("tree/inside");
        let mut fp = pane(&t);
        let idx = fp.entries.iter().position(|e| e.name == "tree").unwrap();
        fp.select(idx);
        fp.start_dialog(DialogKind::Delete(t.0.join("tree"), true));
        fp.dialog_key(key(KeyCode::Char('y')));
        assert!(!t.0.join("tree").exists());
    }

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from(name),
            is_dir,
            size: 0,
        }
    }

    /// The menu a picture gets — the longest one there is, so geometry is
    /// tested against the worst case.
    fn picture_menu() -> Vec<MenuItem> {
        menu_items(Some(&entry("sky.jpg", false)))
    }

    #[test]
    fn a_picture_gets_the_wallpaper_row_and_nothing_else_does() {
        assert!(picture_menu().contains(&MenuItem::SetWallpaper));
        for name in ["notes.txt", "song.mp3", "bundle.tar.gz", "script.sh"] {
            assert!(
                !menu_items(Some(&entry(name, false))).contains(&MenuItem::SetWallpaper),
                "{name} is not a picture"
            );
        }
        assert!(
            !menu_items(Some(&entry("Photos", true))).contains(&MenuItem::SetWallpaper),
            "a folder is not a picture"
        );
        // Nothing selected: only the rows that do not need a file.
        assert_eq!(menu_items(None), vec![MenuItem::NewFolder, MenuItem::Refresh]);
        // `..` is a way out of the folder, not a file to rename or delete.
        assert_eq!(
            menu_items(Some(&entry("..", true))),
            vec![MenuItem::NewFolder, MenuItem::Refresh]
        );
    }

    #[test]
    fn the_menu_never_leaves_the_screen() {
        let screen = Rect::new(0, 0, 40, 12);
        let menu = Menu {
            x: 39,
            y: 11,
            item: 0,
        };
        let r = menu_rect(&menu, &picture_menu(), screen);
        assert!(r.x + r.width <= screen.width);
        assert!(r.y + r.height <= screen.height);

        let menu = Menu {
            x: 0,
            y: 0,
            item: 0,
        };
        let r = menu_rect(&menu, &picture_menu(), screen);
        assert_eq!((r.x, r.y), (0, 0));
    }

    #[test]
    fn every_menu_row_is_clickable_and_nothing_else_is() {
        let screen = Rect::new(0, 0, 80, 24);
        let menu = Menu {
            x: 10,
            y: 5,
            item: 0,
        };
        let items = picture_menu();
        let r = menu_rect(&menu, &items, screen);
        for (i, item) in items.iter().enumerate() {
            let y = r.y + 1 + i as u16;
            assert_eq!(
                menu_item_at(&menu, &items, screen, r.x + 2, y),
                Some(i),
                "{}",
                item.label()
            );
        }
        // The border, and the row just past the last item.
        assert_eq!(menu_item_at(&menu, &items, screen, r.x + 2, r.y), None);
        assert_eq!(
            menu_item_at(&menu, &items, screen, r.x + 2, r.y + 1 + items.len() as u16),
            None
        );
    }

    #[test]
    fn a_click_selects_the_row_under_it_and_a_second_click_opens_it() {
        let t = TempDir::new("click");
        t.file("a.txt");
        t.file("b.txt");
        let mut fp = pane(&t);
        fp.set_rows(10);
        let content = Rect::new(0, 4, 40, 10);
        // Row 0 is "..", so a click on the second row selects "a.txt".
        let ev = |row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(fp.on_mouse(ev(5), content, Rect::new(0, 0, 80, 24)), FileAction::None);
        assert_eq!(fp.selected_entry().map(|e| e.name.as_str()), Some("a.txt"));
        // The same row again within the double-click window opens it.
        assert!(matches!(
            fp.on_mouse(ev(5), content, Rect::new(0, 0, 80, 24)),
            FileAction::Open(..)
        ));
    }

    #[test]
    fn right_click_selects_the_row_and_opens_the_menu_there() {
        let t = TempDir::new("rclick");
        t.file("a.txt");
        let mut fp = pane(&t);
        fp.set_rows(10);
        let content = Rect::new(0, 4, 40, 10);
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 7,
            row: 5,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(fp.on_mouse(ev, content, Rect::new(0, 0, 80, 24)), FileAction::None);
        assert_eq!(fp.selected_entry().map(|e| e.name.as_str()), Some("a.txt"));
        let menu = fp.menu.as_ref().expect("menu opens");
        assert_eq!((menu.x, menu.y), (7, 5));
    }

    #[test]
    fn clicking_a_menu_row_runs_that_action() {
        let t = TempDir::new("menuclick");
        t.file("a.txt");
        let mut fp = pane(&t);
        fp.set_rows(10);
        let screen = Rect::new(0, 0, 80, 24);
        let content = Rect::new(0, 4, 40, 10);
        fp.on_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 7,
                row: 5,
                modifiers: KeyModifiers::empty(),
            },
            content,
            screen,
        );
        let menu = fp.menu.clone().unwrap();
        let items = fp.menu_items();
        let r = menu_rect(&menu, &items, screen);
        let row = items
            .iter()
            .position(|i| *i == MenuItem::OpenInPane)
            .expect("the row is there");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: r.x + 2,
            row: r.y + 1 + row as u16,
            modifiers: KeyModifiers::empty(),
        };
        match fp.on_mouse(click, content, screen) {
            FileAction::OpenInPane(p, is_dir) => {
                assert!(p.ends_with("a.txt"));
                assert!(!is_dir);
            }
            other => panic!("expected OpenInPane, got {other:?}"),
        }
        assert!(fp.menu.is_none());
    }

    #[test]
    fn a_click_outside_the_open_menu_closes_it_without_opening_anything() {
        let t = TempDir::new("menuoutside");
        t.file("a.txt");
        let mut fp = pane(&t);
        fp.set_rows(10);
        let screen = Rect::new(0, 0, 80, 24);
        let content = Rect::new(0, 4, 40, 10);
        fp.open_menu(7, 5);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 20,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(fp.on_mouse(click, content, screen), FileAction::None);
        assert!(fp.menu.is_none());
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 K");
        assert_eq!(human_size(1536), "1.5 K");
        assert_eq!(human_size(10 * 1024), "10 K");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 M");
    }

    #[test]
    fn a_dialog_swallows_typing_so_it_cannot_reach_a_shell() {
        let t = TempDir::new("modal");
        let mut fp = pane(&t);
        fp.start_dialog(DialogKind::NewFolder);
        let action = fp.on_key(key(KeyCode::Char('q')));
        assert_eq!(action, FileAction::None, "q must not close the pane while typing");
        assert!(fp.dialog.is_some());
        assert_eq!(fp.dialog.as_ref().unwrap().input, "q");
    }

    #[test]
    fn shift_f10_asks_for_the_context_menu() {
        let t = TempDir::new("ctxmenu");
        t.file("a.txt");
        let mut fp = pane(&t);
        let ev = KeyEvent::new(KeyCode::F(10), KeyModifiers::SHIFT);
        assert_eq!(fp.on_key(ev), FileAction::ContextMenu);
    }

    #[test]
    fn q_closes_the_pane_when_nothing_else_is_open() {
        let t = TempDir::new("close");
        let mut fp = pane(&t);
        assert_eq!(fp.on_key(key(KeyCode::Char('q'))), FileAction::ClosePane);
    }
}
