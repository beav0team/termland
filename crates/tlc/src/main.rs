//! tlc — talk to a running termland from any shell.
//!
//! Zero dependencies on purpose: this binary is the scriptable surface of the
//! window manager, so it must build and run anywhere, instantly.

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const USAGE: &str = "\
tlc — control a running termland

usage: tlc <command> [args...]

  split [v|h]          split the focused pane (v = side-by-side, h = stacked)
  close                close the focused pane
  focus <h|j|k|l|next|prev>
  resize <h|j|k|l>     nudge the divider
  zoom                 toggle full-width for the focused pane
  run <prog> [args]    new pane running a program
  shell                new pane running $SHELL
  files [dir]          new file manager pane (no dir = $HOME)
  chart                new live chart pane (cpu/ram/io)
  view <list|icons>    switch the focused file pane's view
  desktop [on|off]     show the desktop (wallpaper + ~/Desktop icons)
  wallpaper [path]     set the wallpaper from a picture, or `clear`
  image [path]         new image pane (no path = built-in demo picture)
  web <url>            new pane running w3m on that url
  keys <text>          type into the focused pane; \\r \\t \\e are escapes
  key <chord>          send a tile key, e.g. super+e, ctrl+space, n, enter
  mouse <x> <y> [move|click|right]
                       move the pointer, or click, at a cell position
  list                 one line per visible pane
  workspace <1..10>    switch workspace
  layout               geometry summary
  quit                 exit termland
  ping                 liveness check

exit status is 0 on success, 1 when termland reports an error.
";

/// Resolve the control socket.
///
/// **Must stay in step with `crates/termland/src/ipc.rs`**: the server binds
/// this path and this client connects to it. A `/tmp` fallback here (which
/// this function used to have) makes `tlc` look somewhere the server never
/// binds, and it also trusts a world-writable directory with a socket name.
fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("TERMLAND_SOCKET") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".run")))
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("termland.sock")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{USAGE}");
        std::process::exit(if args.is_empty() { 2 } else { 0 });
    }

    let line = args.join("\t");
    let path = socket_path();

    let stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "tlc: cannot reach termland at {}: {e}\n     (start it with `tl`, or set TERMLAND_SOCKET)",
                path.display()
            );
            std::process::exit(1);
        }
    };

    if let Err(e) = (&stream).write_all(format!("{line}\n").as_bytes()) {
        eprintln!("tlc: write failed: {e}");
        std::process::exit(1);
    }

    let mut out = String::new();
    let mut reader = BufReader::new(&stream);
    let _ = reader.read_to_string(&mut out);
    print!("{out}");
    if out.starts_with("error") {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_falls_back_to_tmp() {
        // Mirrors the same test in the server crate: the two copies of this
        // path logic must agree.
        let prior = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        assert_eq!(socket_path(), PathBuf::from("/run/user/1000/termland.sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert!(!socket_path().starts_with("/tmp"));
        match prior {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }
}
