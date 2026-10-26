use anyhow::Result;
use ratatui_image::picker::Picker;

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;
use termland::app::{build_demo, App};
use termland::surface::KittySurface;
use termland::system::SystemInfo;
use termland::{ipc, log};
use termland::layout::Dir;
use termland::msg::Msg;

/// How often the loop wakes up even with nothing to do: fast enough for a
/// clock that lands on the minute and a half-second cursor blink, slow enough
/// to stay invisible in `top`.
const TICK: Duration = Duration::from_millis(250);
/// The heartbeat while something is animating. 60 fps of a *diffed* cell buffer
/// is a few hundred bytes per frame; 60 fps of a full repaint would not be, so
/// the idle beat stays at 250 ms.
const FRAME: Duration = Duration::from_millis(16);
/// System probes shell out to nmcli/pactl/bluetoothctl. Twice a second would
/// be a lot of process churn for a battery icon.
const PROBE_EVERY: Duration = Duration::from_secs(2);

const USAGE: &str = "\
termland — a tiling window manager for your terminal

usage: termland [options] [-- command [args...]]

options:
  --demo           open the three-pane demo (monitor · editor · image)
  --image <path>   start with an image pane on the right
  -h, --help       this text
  -V, --version    version

keys — Super is the desktop key, the way it is in Hyprland:
  Super+Q  close pane           Super+E  file manager
  Super+R  app menu             Super+V  flip the split
  Super+F  zoom pane            Super+N  new shell
  Super+H J K L / arrows  focus    Super+Shift+H J K L  move pane
  Super+1..9  workspace         Super+Shift+1..9  send pane to workspace
  Super+M  quit                 Super+?  this help

keys — leader Ctrl+Space still works too:
  v |  split side-by-side       s -  split stacked
  h j k l  focus                H J K L  resize
  n  new shell                  i  image    w  web    f/z  zoom
  q x  close pane               1..9  workspace     Tab  cycle

mouse: click focuses, the × on the left closes, drag a divider to resize,
       wheel scrolls, right-click in the file manager for its menu.

control from another shell: tlc <command>   (tlc help)
";

fn main() -> Result<()> {
    log::init();
    ensure_locale();

    let mut demo = false;
    let mut image: Option<PathBuf> = None;
    let mut cmd: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("termland {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--demo" => demo = true,
            "--image" => {
                image = args.next().map(PathBuf::from);
                if image.is_none() {
                    eprintln!("--image needs a path");
                    std::process::exit(2);
                }
            }
            "--" => {
                cmd = args.by_ref().collect();
                break;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown option {other}\n\n{USAGE}");
                std::process::exit(2);
            }
            other => {
                // Bare word: treat the rest as the command to run.
                cmd.push(other.to_string());
                cmd.extend(args.by_ref());
                break;
            }
        }
    }

    let surface = KittySurface::open()?;

    // The picker must ask the terminal about its graphics support *after* raw
    // mode is on, or the reply sits in the cooked line buffer and we hang.
    let picker = match Picker::from_query_stdio() {
        Ok(p) => p,
        Err(e) => {
            let _ = e;
            Picker::halfblocks()
        }
    };

    let (tx, rx) = channel::<Msg>();

    // One reader thread for the host terminal; everything funnels through the
    // same channel as pty output, so the loop is a plain `recv`.
    let input_tx = tx.clone();
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || loop {
            match ratatui::crossterm::event::read() {
                Ok(ev) => {
                    if input_tx.send(Msg::Input(ev)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        })
        .ok();

    // System probes run off the render loop: a network manager that is
    // restarting must not stop the cursor from blinking.
    let probe_tx = tx.clone();
    std::thread::Builder::new()
        .name("sys".into())
        .spawn(move || loop {
            let info = SystemInfo::probe();
            if probe_tx.send(Msg::Sys(Box::new(info))).is_err() {
                break;
            }
            std::thread::sleep(PROBE_EVERY);
        })
        .ok();

    let mut app = App::new(tx.clone(), Some(picker), Box::new(surface));

    if demo {
        build_demo(&mut app, image);
    } else if let Some(ws_defs) = &app.config.workspaces {
        // Spawn configured default workspace apps.
        let ws_defs: Vec<termland::config::WorkspaceDef> = ws_defs.clone();
        for (i, ws_def) in ws_defs.iter().enumerate() {
            if i > 0 {
                app.goto_workspace(i);
            }
            let dir = match ws_def.layout.as_deref() {
                Some("vertical") | Some("stacked") => Dir::Vertical,
                _ => Dir::Horizontal,
            };
            if let Some(apps) = &ws_def.apps {
                for app_cmd in apps {
                    let argv = app.apps.argv_for(app_cmd);
                    if !argv.is_empty() {
                        app.spawn_term(argv, dir);
                        app.focus_dir(termland::layout::NavDir::Left);
                    }
                }
            }
        }
        app.goto_workspace(0);
    } else {
        if cmd.is_empty() {
            cmd = termland::pty::shell_argv();
        }
        app.spawn_term(cmd, Dir::Horizontal);
        if let Some(p) = image {
            app.spawn_image(Some(p), Dir::Horizontal);
            app.focus_dir(termland::layout::NavDir::Left);
        }
    }

    let sock = ipc::spawn_server(tx).ok();

    let res = run(&mut app, rx);

    // cleanup handled by KittySurface::drop
    if let Some(s) = sock {
        let _ = std::fs::remove_file(s);
    }
    res
}

fn run(
    app: &mut App,
    rx: Receiver<Msg>,
) -> Result<()> {
    app.draw();
    loop {
        // Sleep long when nothing moves and short when something does. An idle
        // desktop costs nothing; a hovering pointer gets a smooth fade.
        let wait = if app.animating() { FRAME } else { TICK };
        match rx.recv_timeout(wait) {
            Ok(msg) => app.handle(msg),
            // The heartbeat: repaint, advance the clock, blink the cursor.
            Err(RecvTimeoutError::Timeout) => app.handle(Msg::Tick),
            // Every sender is gone, so nothing can ever arrive again.
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        // Batch whatever piled up behind it so a chatty pane cannot starve the
        // renderer, but also cannot force one draw per byte.
        for _ in 0..256 {
            match rx.try_recv() {
                Ok(msg) => app.handle(msg),
                Err(_) => break,
            }
        }
        if app.quit {
            return Ok(());
        }
        app.draw();
    }
}

/// A distro frontend has to survive an empty environment. With no `LANG` the
/// libc locale is `C`, and every ncurses app that draws box characters or a
/// non-ASCII filename renders mojibake or refuses to start. `C.UTF-8` is
/// present on every modern glibc and needs no locale generation.
fn ensure_locale() {
    for key in ["LANG", "LC_CTYPE", "LC_ALL"] {
        let usable = std::env::var(key)
            .map(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("utf-8") || v.contains("utf8")
            })
            .unwrap_or(false);
        if !usable {
            std::env::set_var(key, "C.UTF-8");
        }
    }
}
