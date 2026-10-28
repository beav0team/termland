//! termland — a tiling window manager that lives inside one terminal tab.
//!
//! The pieces:
//! * [`layout`] — binary split tree (dwindle), geometry, directional focus
//! * [`pty`] — child processes in ptys, parsed with `vt100`
//! * [`image`] — real images via the kitty graphics protocol
//! * [`keys`] — leader detection and the ANSI key encoder
//! * [`mouse`] — child mouse-tracking detection and event encoding
//! * [`images`] — placements, transmit-once, and the frame's image layer
//! * [`anim`] — frame clock, easing and springs: motion that can be tested
//! * [`widgets`] — control ids, hover, press and the animated hover amount
//! * [`monitor`] — the live system monitor: `sysinfo` samples, `tui-chart` draws
//! * [`desktop`] — wallpaper and the icons of `~/Desktop`
//! * [`files`] — the built-in file manager pane
//! * [`ipc`] — the `tlc` control socket
//! * [`log`] — append-only error log, no dependencies
//! * [`app`] — state, keymap, and the draw loop
//! * [`config`] — config.toml loading, defaults, colors
//! * [`surface`] — frontend abstraction: kitty now, GPU/DRM later
//! * [`wallpaper`] — a picture behind the desktop, sampled into half blocks
//! * [`system`] — battery, wifi, bluetooth, volume, clock info for the panel
//! * [`panel`] — top control bar (desktop-manager style)
//! * [`apps`] — app registry: standalone, updatable, deletable programs

pub mod anim;
pub mod app;
pub mod apps;
pub mod config;
pub mod desktop;
pub mod files;
pub mod icons;
pub mod image;
pub mod images;
pub mod ipc;
pub mod keys;
pub mod layout;
pub mod log;
pub mod monitor;
pub mod mouse;
pub mod msg;
pub mod panel;
pub mod pty;
pub mod surface;
pub mod system;
pub mod theme;
pub mod ui;
pub mod wallpaper;
pub mod widgets;

pub use app::App;
pub use apps::AppRegistry;
pub use config::{Config, WorkspaceDef};
pub use msg::{IpcRequest, Msg};
pub use panel::Panel;
pub use surface::{Frame, InputEvent, KittySurface, Surface};
pub use system::SystemInfo;
