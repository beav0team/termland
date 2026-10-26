//! System information for the top panel: battery, volume, wifi, bluetooth,
//! clock, and the list of open panes.
//!
//! Reads from /sys/class, nmcli, pactl, bluetoothctl — whatever is available
//! on the host. Falls back gracefully when a tool isn't present.
//!
//! Two rules here exist because this panel runs on a *live* loop:
//!
//! * every external command is bounded by [`run_with_timeout`], so a wedged
//!   `bluetoothctl` or a NetworkManager that is busy can never freeze the UI;
//! * the clock is read with `localtime_r`, because the panel is not a log
//!   line — a user in Casablanca must see their wall clock, not UTC.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long any one probe command may take. The panel refreshes every couple
/// of seconds; a probe that outlives this is one we would never display.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(700);

/// Battery state read from /sys/class/power_supply.
#[derive(Clone, Debug, Default)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// Network/wifi state read from nmcli.
#[derive(Clone, Debug, Default)]
pub struct Network {
    pub ssid: Option<String>,
    pub signal: u8,
    pub connected: bool,
}

/// Bluetooth state read from bluetoothctl.
#[derive(Clone, Debug, Default)]
pub struct Bluetooth {
    pub powered: bool,
    pub connected_devices: u32,
}

/// Audio volume read from pactl.
#[derive(Clone, Debug, Default)]
pub struct Audio {
    pub percent: u8,
    pub muted: bool,
}

/// All system info gathered at once.
#[derive(Clone, Debug, Default)]
pub struct SystemInfo {
    pub battery: Battery,
    pub network: Network,
    pub bluetooth: Bluetooth,
    pub audio: Audio,
    pub clock: String,
}

/// Read a single line from a sysfs file.
fn read_sysfs(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Run `prog` and return its stdout, or `None` if it is missing, fails, or
/// takes longer than `timeout`.
///
/// The child's stdout is drained on a helper thread rather than with
/// `wait_with_output`: a process that fills the pipe buffer would otherwise
/// deadlock against our `kill`, and the timeout would never fire.
pub fn run_with_timeout(prog: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });

    match rx.recv_timeout(timeout) {
        Ok(out) => {
            let _ = child.wait();
            Some(out)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// The wall clock as `HH:MM` in the user's own timezone.
///
/// `SystemTime` is UTC, so formatting it directly (the old behaviour) showed
/// the wrong hour everywhere except UTC. `localtime_r` is the libc call that
/// consults `/etc/localtime` and `TZ`, and it is re-entrant, unlike
/// `localtime`.
pub fn local_clock() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `time` and `localtime_r` are plain libc calls; `tm` is a
        // zeroed out-parameter and the result pointer is checked before use.
        unsafe {
            let mut now: libc::time_t = 0;
            if libc::time(&mut now) == -1 {
                return "--:--".to_string();
            }
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return "--:--".to_string();
            }
            return format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
        }
    }
    #[cfg(not(unix))]
    {
        "--:--".to_string()
    }
}

impl Battery {
    pub fn probe() -> Self {
        let mut percent = 0u8;
        let mut charging = false;

        // Try BAT0 first, then BAT1.
        for bat in ["BAT0", "BAT1"] {
            let cap_path = Path::new("/sys/class/power_supply").join(bat).join("capacity");
            if let Some(cap) = read_sysfs(&cap_path) {
                percent = cap.parse().unwrap_or(0);
            }
            let status_path = Path::new("/sys/class/power_supply").join(bat).join("status");
            if let Some(status) = read_sysfs(&status_path) {
                charging = status.eq_ignore_ascii_case("Charging");
            }
            if percent > 0 {
                break;
            }
        }

        Battery { percent, charging }
    }
}

impl Network {
    pub fn probe() -> Self {
        // Try nmcli first.
        if let Some(text) = run_with_timeout(
            "nmcli",
            &["-t", "-f", "ACTIVE,SSID,SIGNAL", "device", "wifi", "list"],
            PROBE_TIMEOUT,
        ) {
            for line in text.lines() {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() >= 3 && parts[0] == "yes" {
                    return Network {
                        ssid: Some(parts[1].to_string()),
                        signal: parts[2].parse().unwrap_or(0),
                        connected: true,
                    };
                }
            }
        }

        // Fallback: check if any wifi interface is up via /sys/class/net.
        if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("wlan") || name.starts_with("wlp") {
                    let operstate_path =
                        Path::new("/sys/class/net").join(&name).join("operstate");
                    if let Some(state) = read_sysfs(&operstate_path) {
                        if state == "up" {
                            return Network {
                                connected: true,
                                ..Default::default()
                            };
                        }
                    }
                }
            }
        }

        Network::default()
    }
}

impl Bluetooth {
    pub fn probe() -> Self {
        if let Some(text) = run_with_timeout("bluetoothctl", &["show"], PROBE_TIMEOUT) {
            let powered = text.contains("Powered: yes");
            let connected = text
                .lines()
                .filter(|l| l.contains("Connected: yes"))
                .count() as u32;
            return Bluetooth {
                powered,
                connected_devices: connected,
            };
        }

        Bluetooth::default()
    }
}

impl Audio {
    pub fn probe() -> Self {
        // Try pactl for the default sink.
        if let Some(text) = run_with_timeout(
            "pactl",
            &["get-sink-volume", "@DEFAULT_SINK@"],
            PROBE_TIMEOUT,
        ) {
            // Pactl output: "Volume: 0: 45%  1: 45%"
            if let Some(vol_part) = text.split("Volume:").nth(1) {
                if let Some(pct) = vol_part.split('%').next() {
                    let digits: String = pct.chars().filter(|c| c.is_ascii_digit()).collect();
                    if let Ok(percent) = digits.parse::<u8>() {
                        let muted = run_with_timeout(
                            "pactl",
                            &["get-sink-mute", "@DEFAULT_SINK@"],
                            PROBE_TIMEOUT,
                        )
                        .map(|o| o.contains("yes"))
                        .unwrap_or(false);
                        return Audio {
                            percent: percent.min(100),
                            muted,
                        };
                    }
                }
            }
        }

        Audio::default()
    }
}

impl SystemInfo {
    /// Gather all system info in one shot.
    ///
    /// This is called from a background thread, not the render loop; the
    /// individual probes are what bound its cost.
    pub fn probe() -> Self {
        Self {
            battery: Battery::probe(),
            network: Network::probe(),
            bluetooth: Bluetooth::probe(),
            audio: Audio::probe(),
            clock: local_clock(),
        }
    }
}

/// Format a short status string for the panel.
impl SystemInfo {
    pub fn wifi_icon(&self) -> &'static str {
        if !self.network.connected {
            "◌"
        } else if self.network.signal >= 75 {
            "▂▄▆█"
        } else if self.network.signal >= 50 {
            "▂▄▆"
        } else if self.network.signal >= 25 {
            "▂▄"
        } else {
            "▂"
        }
    }

    pub fn battery_icon(&self) -> String {
        let p = self.battery.percent;
        let chg = if self.battery.charging { "⚡" } else { "" };
        let icon = if p >= 90 {
            "█"
        } else if p >= 70 {
            "▇"
        } else if p >= 50 {
            "▆"
        } else if p >= 30 {
            "▅"
        } else if p >= 15 {
            "▃"
        } else {
            "▂"
        };
        format!("{icon}{chg}")
    }

    pub fn volume_icon(&self) -> &'static str {
        if self.audio.muted || self.audio.percent == 0 {
            "🔇"
        } else if self.audio.percent >= 70 {
            "🔊"
        } else if self.audio.percent >= 30 {
            "🔉"
        } else {
            "🔈"
        }
    }

    pub fn bluetooth_icon(&self) -> &'static str {
        if !self.bluetooth.powered {
            "◌"
        } else if self.bluetooth.connected_devices > 0 {
            "🔗"
        } else {
            "Bluetooth"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn battery_icon_returns_something() {
        let sys = SystemInfo {
            battery: Battery {
                percent: 50,
                charging: false,
            },
            ..Default::default()
        };
        assert!(!sys.battery_icon().is_empty());
    }

    #[test]
    fn wifi_icon_returns_something() {
        let sys = SystemInfo {
            network: Network {
                connected: true,
                signal: 80,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!sys.wifi_icon().is_empty());
    }

    #[test]
    fn local_clock_is_hh_mm() {
        let c = local_clock();
        let parts: Vec<&str> = c.split(':').collect();
        assert_eq!(parts.len(), 2, "clock {c:?} is not HH:MM");
        assert_eq!(parts[0].len(), 2);
        assert_eq!(parts[1].len(), 2);
        assert!(parts[0].parse::<u8>().unwrap() < 24);
        assert!(parts[1].parse::<u8>().unwrap() < 60);
    }

    #[test]
    fn run_with_timeout_captures_stdout() {
        let out = run_with_timeout("sh", &["-c", "printf hi"], Duration::from_secs(2));
        assert_eq!(out.as_deref(), Some("hi"));
    }

    #[test]
    fn run_with_timeout_gives_up_on_a_slow_command() {
        let start = Instant::now();
        let out = run_with_timeout("sleep", &["5"], Duration::from_millis(50));
        assert!(out.is_none());
        // The point of the bound: we must not wait for `sleep`.
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn run_with_timeout_is_none_for_a_missing_program() {
        assert!(run_with_timeout("termland-no-such-binary", &[], PROBE_TIMEOUT).is_none());
    }
}
