//! The `tlc` control channel: a unix socket carrying one tab-separated line
//! per request (so paths with spaces need no quoting) and a plain-text reply.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::msg::{IpcRequest, Msg};

/// The socket path, given the two directories it can live in.
///
/// Pure so the decision is testable without touching the environment.
pub fn socket_path_in(runtime: Option<&Path>, home: &Path) -> PathBuf {
    match runtime {
        Some(dir) => dir.join("termland.sock"),
        // `/tmp` is world-writable and shared: a socket there can be squatted
        // by any local user. `$HOME/.run` is ours and stays 0700.
        None => home.join(".run/termland.sock"),
    }
}

pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("TERMLAND_SOCKET") {
        return PathBuf::from(p);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    socket_path_in(runtime.as_deref(), &home)
}

/// Create the socket directory if needed, refusing one that is not ours.
fn prepare_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            bail!("{} is not a directory", dir.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: geteuid has no preconditions and cannot fail.
            let euid = unsafe { libc::geteuid() };
            if meta.uid() != euid {
                bail!("{} is not owned by uid {euid}", dir.display());
            }
        }
    } else {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Remove a stale socket, but never follow a symlink or delete somebody
/// else's file. Only called after the parent directory has been checked.
fn clear_stale(path: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt;
    if meta.file_type().is_symlink() {
        bail!("refusing to remove symlink at {}", path.display());
    }
    if !meta.file_type().is_socket() && !meta.is_file() {
        bail!("refusing to remove non-socket at {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: as above.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            bail!("refusing to remove {}: not owned by us", path.display());
        }
    }
    std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
}

pub fn parse(line: &str) -> IpcRequest {
    let mut it = line.trim_end_matches(['\n', '\r']).split('\t');
    let cmd = it.next().unwrap_or("").to_string();
    IpcRequest {
        cmd,
        args: it.map(|s| s.to_string()).collect(),
    }
}

pub fn connect() -> Result<UnixStream> {
    let path = socket_path();
    UnixStream::connect(&path).with_context(|| {
        format!(
            "no termland listening on {} (is it running?)",
            path.display()
        )
    })
}

/// Send one request and read the whole reply.
pub fn request(line: &str) -> Result<String> {
    let mut s = connect()?;
    s.write_all(line.as_bytes())?;
    s.write_all(b"\n")?;
    s.flush()?;
    let mut out = String::new();
    BufReader::new(&s).read_to_string(&mut out)?;
    Ok(out)
}

pub fn spawn_server(tx: Sender<Msg>) -> Result<PathBuf> {
    let path = socket_path();
    let Some(dir) = path.parent() else {
        bail!("socket path {} has no parent", path.display());
    };
    prepare_dir(dir)?;
    clear_stale(&path)?;
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind {}", path.display()))?;

    // The default umask leaves the socket 0775, which lets every local user
    // drive this terminal through `tlc`. Bind creates it before we can say
    // otherwise, so tighten it immediately.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod {}", path.display()))?;
    }

    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                // One request per connection; cheap and stateless.
                let _ = handle(stream, &tx);
            }
        })
        .context("spawn ipc thread")?;

    Ok(path)
}

fn handle(stream: UnixStream, tx: &Sender<Msg>) -> Result<()> {
    let mut line = String::new();
    {
        let mut r = BufReader::new(&stream);
        r.read_line(&mut line)?;
    }
    if line.trim().is_empty() {
        return Ok(());
    }
    let req = parse(&line);
    let (rtx, rrx) = std::sync::mpsc::channel::<String>();
    if tx.send(Msg::Ipc(req, rtx)).is_err() {
        let mut s = &stream;
        let _ = s.write_all(b"error app gone\n");
        return Ok(());
    }
    let reply = rrx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| "error timeout\n".to_string());
    let mut s = &stream;
    let _ = s.write_all(reply.as_bytes());
    Ok(())
}

pub fn decode_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_runtime_dir() {
        let p = socket_path_in(Some(Path::new("/run/user/1000")), Path::new("/home/x"));
        assert_eq!(p, Path::new("/run/user/1000/termland.sock"));
    }

    #[test]
    fn never_falls_back_to_tmp() {
        let p = socket_path_in(None, Path::new("/home/x"));
        assert_eq!(p, Path::new("/home/x/.run/termland.sock"));
        assert!(!p.starts_with("/tmp"));
    }

    #[test]
    fn prepare_dir_creates_a_private_directory() {
        let base = std::env::temp_dir().join(format!("termland-ipc-{}", std::process::id()));
        let dir = base.join("run");
        let _ = std::fs::remove_dir_all(&base);
        prepare_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "socket dir must not be group/world writable");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn clear_stale_refuses_a_symlink() {
        let base = std::env::temp_dir().join(format!("termland-ipc-sym-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let target = base.join("target");
        std::fs::write(&target, "do not delete").unwrap();
        let link = base.join("termland.sock");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(clear_stale(&link).is_err());
        assert!(target.exists(), "the symlink target must survive");
        assert!(link.symlink_metadata().is_ok(), "the link itself must survive");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parse_is_tab_separated() {
        let r = parse("spawn\tsleep\t5\n");
        assert_eq!(r.cmd, "spawn");
        assert_eq!(r.args, vec!["sleep", "5"]);
    }
}
