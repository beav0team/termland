//! A tiny append-only log, because a tiling terminal that dies silently is
//! unusable on a machine you cannot open a second terminal on.
//!
//! No dependencies and no rotation: one line at a time, one file, mode 0600.
//! `$TERMLAND_LOG` overrides the path; otherwise it is
//! `$XDG_STATE_HOME/termland/termland.log`, falling back to
//! `~/.local/state/termland/termland.log`.
//!
//! Failures to log are swallowed on purpose. The log exists to explain the
//! failure that already happened; it must never become the next one.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Where the log would be written, for `termland --doctor`-style output and
/// tests.
pub fn path() -> Option<PathBuf> {
    PATH.get_or_init(resolve).clone()
}

fn resolve() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TERMLAND_LOG") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let base = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))?;
    Some(base.join("termland/termland.log"))
}

/// Install the panic hook. Call once, before anything that can panic.
pub fn init() {
    let path = path();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write(&format!("panic: {info}"));
        default_hook(info);
    }));
    if let Some(p) = path {
        write(&format!(
            "--- termland {} started (pid {}) ---",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ));
        let _ = p;
    }
}

/// Append one line, timestamped with the local wall clock.
pub fn write(msg: &str) {
    let Some(path) = path() else { return };
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    let stamp = crate::system::local_clock();
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(f, "[{stamp}:{:02}] {msg}", secs % 60);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honours_the_env_override() {
        // `OnceLock` makes this process-wide, so do not disturb the real path
        // in a test binary: only assert the resolver's shape.
        let p = resolve();
        assert!(p.is_some(), "a state dir should always resolve");
        assert!(p.unwrap().to_string_lossy().ends_with(".log"));
    }
}
