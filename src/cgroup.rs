use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Where a systemd user unit sits: GNOME puts launched apps in `app.slice`,
/// the shell and its helpers in `session.slice`, indexers in `background.slice`.
const SLICES: [(&str, &str); 3] = [
    ("app.slice", "app"),
    ("session.slice", "session"),
    ("background.slice", "background"),
];

/// Turns a systemd unit name into a desktop-ish app id.
///
/// `app-gnome-codium-7128.scope` -> `codium`,
/// `app-com.google.Chrome-11619.scope` -> `com.google.Chrome`,
/// `app-gnome\x2dsession\x2dmanager.slice` -> `gnome-session-manager`,
/// `snap.firefox.firefox-<uuid>.scope` -> `firefox`.
pub fn parse_app_id(name: &str) -> Option<String> {
    if let Some(rest) = name.strip_prefix("snap.") {
        return rest
            .split('.')
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    let id = if let Some(rest) = name.strip_prefix("app-") {
        if let Some(slice) = rest.strip_suffix(".slice") {
            unescape(slice)
        } else {
            let (body, is_scope) = if let Some(b) = rest.strip_suffix(".scope") {
                (b, true)
            } else {
                let b = rest.strip_suffix(".service")?;
                (b, false)
            };
            let body = body.split('@').next()?;
            let parts: Vec<&str> = body.split('-').collect();
            let numeric_tail = parts.len() > 1 && parts.last()?.chars().all(|c| c.is_ascii_digit());
            let end = if is_scope && numeric_tail {
                parts.len() - 1
            } else {
                parts.len()
            };
            match end {
                0 => return None,
                1 => unescape(parts[0]),
                _ => unescape(&parts[1..end].join("-")),
            }
        }
    } else {
        let base = name
            .strip_suffix(".service")
            .or_else(|| name.strip_suffix(".scope"))
            .unwrap_or(name);
        unescape(base.split('@').next()?)
    };
    Some(strip_dbus_prefix(&id).to_string()).filter(|s| !s.is_empty())
}

fn unescape(s: &str) -> String {
    s.replace("\\x2d", "-")
}

/// `dbus-:1.2-org.gnome.Nautilus` -> `org.gnome.Nautilus`.
fn strip_dbus_prefix(id: &str) -> &str {
    id.strip_prefix("dbus-:")
        .and_then(|r| r.split_once('-').map(|(_, rest)| rest))
        .unwrap_or(id)
}

/// Memory a cgroup holds that cannot simply be dropped: everything except
/// file-backed page cache (shared memory stays counted). Matches the system
/// "used" figure, which also leaves reclaimable cache out.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemStat {
    pub mem: u64,
    pub anon: u64,
    pub shmem: u64,
}

pub fn parse_mem(current: u64, stat: &str) -> MemStat {
    let file = stat_value(stat, "file").unwrap_or(0);
    let shmem = stat_value(stat, "shmem").unwrap_or(0);
    MemStat {
        mem: current.saturating_sub(file.saturating_sub(shmem)),
        anon: stat_value(stat, "anon").unwrap_or(0),
        shmem,
    }
}

pub fn read_mem(dir: &Path) -> MemStat {
    let current = fs::read_to_string(dir.join("memory.current"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let stat = fs::read_to_string(dir.join("memory.stat")).unwrap_or_default();
    parse_mem(current, &stat)
}

pub fn read_cpu_usec(dir: &Path) -> u64 {
    fs::read_to_string(dir.join("cpu.stat"))
        .ok()
        .and_then(|s| stat_value(&s, "usage_usec"))
        .unwrap_or(0)
}

fn stat_value(s: &str, key: &str) -> Option<u64> {
    s.lines().find_map(|l| {
        let (k, v) = l.split_once(' ')?;
        (k == key).then(|| v.trim().parse().ok()).flatten()
    })
}

/// The systemd user unit a cgroup path belongs to: its slice kind
/// (`app`, `session`, `background`) and the unit directory name right below
/// the slice. Nested cgroups (terminal tabs, slices) map to that top unit.
pub fn unit_from_cgroup_path(path: &str) -> Option<(&'static str, String)> {
    let mut parts = path.split('/');
    while let Some(p) = parts.next() {
        if let Some((_, kind)) = SLICES.iter().find(|(slice, _)| *slice == p) {
            return parts
                .next()
                .filter(|n| !n.is_empty())
                .map(|n| (*kind, n.to_string()));
        }
    }
    None
}

/// Usage of one top-level user unit (scope, service or slice).
#[derive(Debug, Clone)]
pub struct UnitUsage {
    pub name: String,
    pub id: String,
    pub kind: &'static str,
    pub mem: MemStat,
    pub cpu: f64,
}

#[derive(Default)]
pub struct UnitScanner {
    prev: HashMap<PathBuf, u64>,
}

impl UnitScanner {
    pub fn scan(
        &mut self,
        cgroup_root: &Path,
        uid: u32,
        elapsed_secs: f64,
    ) -> HashMap<String, UnitUsage> {
        let base = cgroup_root
            .join("user.slice")
            .join(format!("user-{uid}.slice"))
            .join(format!("user@{uid}.service"));
        let mut units = HashMap::new();
        let mut next = HashMap::new();
        for (slice, kind) in SLICES {
            let Ok(entries) = fs::read_dir(base.join(slice)) else {
                continue;
            };
            for e in entries.flatten() {
                if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let Ok(name) = e.file_name().into_string() else {
                    continue;
                };
                let Some(id) = parse_app_id(&name) else {
                    continue;
                };
                let dir = e.path();
                let usage = read_cpu_usec(&dir);
                let cpu = match self.prev.get(&dir) {
                    Some(&prev) if elapsed_secs > 0.0 => {
                        usage.saturating_sub(prev) as f64 / (elapsed_secs * 1e6) * 100.0
                    }
                    _ => 0.0,
                };
                next.insert(dir.clone(), usage);
                let mem = read_mem(&dir);
                units.insert(
                    name.clone(),
                    UnitUsage {
                        name,
                        id,
                        kind,
                        mem,
                        cpu,
                    },
                );
            }
        }
        self.prev = next;
        units
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids() {
        let cases = [
            ("app-gnome-codium-7128.scope", "codium"),
            ("app-gnome-dev.warp.Warp-5572.scope", "dev.warp.Warp"),
            ("app-com.google.Chrome-11619.scope", "com.google.Chrome"),
            ("app-com.mitchellh.ghostty.service", "com.mitchellh.ghostty"),
            (
                "app-gnome-at\\x2dspi\\x2ddbus\\x2dbus-3458.scope",
                "at-spi-dbus-bus",
            ),
            (
                "app-gnome\\x2dsession\\x2dmanager.slice",
                "gnome-session-manager",
            ),
            ("app-todoist-7290.scope", "todoist"),
            (
                "app-flatpak-org.mozilla.firefox-12345.scope",
                "org.mozilla.firefox",
            ),
            (
                "app-gnome-org.gnome.SettingsDaemon.Sharing@autostart.service",
                "org.gnome.SettingsDaemon.Sharing",
            ),
            (
                "app-dbus\\x2d:1.2\\x2dorg.gnome.Nautilus.slice",
                "org.gnome.Nautilus",
            ),
            ("snap.firefox.firefox-0a1b.scope", "firefox"),
            ("org.gnome.Shell@wayland.service", "org.gnome.Shell"),
            ("pipewire.service", "pipewire"),
        ];
        for (name, want) in cases {
            assert_eq!(parse_app_id(name).as_deref(), Some(want), "{name}");
        }
    }

    #[test]
    fn cgroup_path_to_unit() {
        let p =
            "/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-codium-7128.scope";
        assert_eq!(
            unit_from_cgroup_path(p),
            Some(("app", "app-gnome-codium-7128.scope".into()))
        );
        let nested = "/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.gnome.Terminal.slice/vte-spawn-1.scope";
        assert_eq!(
            unit_from_cgroup_path(nested).unwrap().1,
            "app-org.gnome.Terminal.slice"
        );
        let shell = "/user.slice/user-1000.slice/user@1000.service/session.slice/org.gnome.Shell@wayland.service";
        assert_eq!(unit_from_cgroup_path(shell).unwrap().0, "session");
        assert_eq!(unit_from_cgroup_path("/system.slice/docker.service"), None);
    }

    #[test]
    fn memory_excludes_page_cache_but_keeps_shmem() {
        let stat = "anon 100\nfile 50\nshmem 20\ninactive_file 5\n";
        let m = parse_mem(200, stat);
        assert_eq!(
            m,
            MemStat {
                mem: 170,
                anon: 100,
                shmem: 20
            }
        );
        assert_eq!(stat_value(stat, "missing"), None);
    }
}
