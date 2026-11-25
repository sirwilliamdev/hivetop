//! Credits each process to the app it belongs to.
//!
//! cgroups alone get this wrong in two ways:
//! - GNOME moves a launched app into its scope only after spawning it, so what
//!   the app forks in between (a browser's zygote and renderers) stays in GNOME
//!   Shell's cgroup.
//! - Portals open a second, launcher-less scope for the same app
//!   (`app-org.chromium.Chromium-<pid>.scope` next to
//!   `app-gnome-brave\x2dbrowser-<pid>.scope`).
//!
//! So a process outside any app scope goes to its nearest ancestor's app, a
//! process in a portal scope goes to the launcher scope it descends from, and
//! portal scopes named after the same PID as a launcher scope are aliases.
//!
//! An app can also leave GNOME Shell's cgroup for the user manager before its
//! scope exists, and then never get one (Sublime Text forks and detaches). Such
//! a process, when a launcher entry carries its name, becomes its own app unit
//! together with its children, instead of counting as GNOME Shell.
//!
//! An app can also start helper units of its own, outside any launcher (Orca's
//! terminal daemon scope and the slice holding its terminals). Such a unit,
//! when a process of another app forked its first process, counts as that app.
//!
//! Processes already listed in the AI Agents section are then taken out of their
//! app, so the menu shows each process once.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cgroup::UnitUsage;
use crate::procscan::Proc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnitRef {
    pub kind: &'static str,
    pub name: String,
    pub id: String,
    /// `app-<id>-<pid>.scope`, created by a portal rather than a launcher.
    pub portal: bool,
    /// Trailing PID of a scope name.
    pub pid: Option<u32>,
}

impl UnitRef {
    pub fn new(kind: &'static str, name: String, id: String) -> Self {
        let (portal, pid) = scope_shape(&name);
        Self {
            kind,
            name,
            id,
            portal,
            pid,
        }
    }
}

/// `(portal, pid)` for `app-[<launcher>-]<id>-<pid>.scope`.
fn scope_shape(name: &str) -> (bool, Option<u32>) {
    let Some(body) = name
        .strip_prefix("app-")
        .and_then(|r| r.strip_suffix(".scope"))
    else {
        return (false, None);
    };
    let parts: Vec<&str> = body.split('-').collect();
    let pid = parts.last().and_then(|p| p.parse::<u32>().ok());
    (pid.is_some() && parts.len() == 2, pid)
}

/// Portal scope name -> launcher scope name, for scopes sharing a trailing PID.
pub fn aliases<'a>(units: impl Iterator<Item = &'a UnitRef>) -> HashMap<String, String> {
    let mut launchers: HashMap<u32, &str> = HashMap::new();
    let mut portals: Vec<(&str, u32)> = Vec::new();
    for u in units {
        match (u.kind, u.portal, u.pid) {
            ("app", false, Some(pid)) => {
                launchers.insert(pid, &u.name);
            }
            ("app", true, Some(pid)) => portals.push((&u.name, pid)),
            _ => {}
        }
    }
    portals
        .into_iter()
        .filter_map(|(name, pid)| Some((name.to_string(), launchers.get(&pid)?.to_string())))
        .collect()
}

/// Unit name -> unit of the app that made it, for helper units an app starts
/// for itself rather than through a launcher (Orca runs its terminal daemon in
/// `orca-daemon-<uuid>.scope` and the terminals in `orca_ptys_<daemon>.slice`).
/// A unit is adopted when its oldest process was forked by a process of
/// another app unit, its name is not a launcher's (`app-*`, `snap.*`) and no
/// launcher entry names it: an app opened from another app keeps its own row.
/// Chains resolve to the last app (the terminals go to Orca, not its daemon).
/// `unit_of` must already have portal aliases resolved.
pub fn adopted(
    procs: &HashMap<i32, Proc>,
    unit_of: &HashMap<i32, Arc<UnitRef>>,
    is_launcher: &dyn Fn(&str) -> bool,
) -> HashMap<String, String> {
    let mut oldest: HashMap<&str, (u64, i32)> = HashMap::new();
    for (pid, unit) in unit_of {
        if let Some(p) = procs.get(pid) {
            let e = oldest.entry(unit.name.as_str()).or_insert((p.start, *pid));
            if (p.start, *pid) < *e {
                *e = (p.start, *pid);
            }
        }
    }
    let mut parent: HashMap<String, String> = HashMap::new();
    for (name, (_, pid)) in &oldest {
        let unit = &unit_of[pid];
        if unit.kind != "app"
            || name.starts_with("app-")
            || name.starts_with("snap.")
            || is_launcher(&unit.id)
        {
            continue;
        }
        let Some(by) = procs.get(pid).and_then(|p| unit_of.get(&p.ppid)) else {
            continue;
        };
        if by.kind == "app" && by.name != unit.name {
            parent.insert(name.to_string(), by.name.clone());
        }
    }
    let mut out = HashMap::with_capacity(parent.len());
    for from in parent.keys() {
        let mut to = &parent[from];
        for _ in 0..8 {
            match parent.get(to) {
                Some(next) => to = next,
                None => break,
            }
        }
        if to != from && !parent.contains_key(to) {
            out.insert(from.clone(), to.clone());
        }
    }
    out
}

/// Name prefix of the app units made for detached processes. Real unit names
/// end in `.scope`, `.service` or `.slice`, so they never collide.
pub const DETACHED_PREFIX: &str = "detached-";

/// Unit each process is accounted to, keyed by PID. `unit_of` must already
/// have aliases resolved. `entry_for` names the launcher entry of a process
/// name, if any.
pub fn assign(
    procs: &HashMap<i32, Proc>,
    unit_of: &HashMap<i32, Arc<UnitRef>>,
    user_manager: Option<i32>,
    entry_for: &dyn Fn(&str) -> Option<String>,
) -> HashMap<i32, Arc<UnitRef>> {
    let nearest = |pid: i32, wanted: &dyn Fn(&UnitRef) -> bool| -> Option<(i32, Arc<UnitRef>)> {
        let mut cur = procs.get(&pid)?.ppid;
        for _ in 0..64 {
            if cur <= 1 {
                return None;
            }
            if let Some(u) = unit_of.get(&cur).filter(|u| wanted(u)) {
                return Some((cur, u.clone()));
            }
            cur = procs.get(&cur)?.ppid;
        }
        None
    };
    let lift = |pid: i32, unit: Arc<UnitRef>| -> Arc<UnitRef> {
        if !unit.portal {
            return unit;
        }
        nearest(pid, &|u| u.kind == "app" && !u.portal)
            .map(|(_, u)| u)
            .unwrap_or(unit)
    };
    let mut app_units: Vec<&Arc<UnitRef>> = unit_of
        .values()
        .filter(|u| u.kind == "app" && !u.portal)
        .collect();
    app_units.sort_by(|a, b| a.name.cmp(&b.name));
    app_units.dedup();
    let mut main_of: HashMap<&str, (u64, i32)> = HashMap::new();
    for (pid, unit) in unit_of {
        if let Some(p) = procs.get(pid) {
            let e = main_of.entry(unit.name.as_str()).or_insert((p.start, *pid));
            if p.start < e.0 {
                *e = (p.start, *pid);
            }
        }
    }
    let reparented = |pid: i32, own: &UnitRef| {
        procs
            .get(&pid)
            .is_some_and(|p| Some(p.ppid) == user_manager)
            && main_of
                .get(own.name.as_str())
                .is_some_and(|(_, main)| *main != pid)
    };
    let mut detached_units: HashMap<String, Arc<UnitRef>> = HashMap::new();
    let mut detached: HashMap<i32, Arc<UnitRef>> = HashMap::new();
    for (pid, own) in unit_of {
        if own.kind == "app" || !reparented(*pid, own) {
            continue;
        }
        let Some(p) = procs.get(pid) else {
            continue;
        };
        if match_by_name(&p.comm, &app_units).is_some() {
            continue;
        }
        if let Some(id) = entry_for(&p.comm) {
            let unit = detached_units
                .entry(id.clone())
                .or_insert_with(|| {
                    Arc::new(UnitRef::new("app", format!("{DETACHED_PREFIX}{id}"), id))
                })
                .clone();
            detached.insert(*pid, unit);
        }
    }
    let detached_root = |pid: i32, own: &UnitRef| -> Option<Arc<UnitRef>> {
        let mut cur = pid;
        for _ in 0..64 {
            if let Some(u) = detached.get(&cur) {
                return Some(u.clone());
            }
            cur = procs.get(&cur)?.ppid;
            if unit_of.get(&cur)?.name != own.name {
                return None;
            }
        }
        None
    };
    let mut out = HashMap::with_capacity(unit_of.len());
    for (pid, own) in unit_of {
        let target = if own.kind == "app" {
            lift(*pid, own.clone())
        } else if let Some((ancestor, unit)) = nearest(*pid, &|u| u.kind == "app") {
            lift(ancestor, unit)
        } else if let Some(unit) = detached_root(*pid, own) {
            unit
        } else {
            reparented(*pid, own)
                .then(|| {
                    procs
                        .get(pid)
                        .and_then(|p| match_by_name(&p.comm, &app_units))
                })
                .flatten()
                .unwrap_or_else(|| own.clone())
        };
        out.insert(*pid, target);
    }
    out
}

/// Memory and CPU taken out of one unit by [`carve`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Carved {
    pub mem: u64,
    pub cpu: f64,
    pub procs: usize,
}

/// Takes processes shown in another section out of the unit that owns them.
/// Each entry is `(unit name, RssAnon + RssShmem, cpu)`. Memory taken is
/// capped by what the unit holds, so an estimate larger than the cgroup
/// leaves it at zero instead of wrapping.
pub fn carve<'a>(
    units: &mut HashMap<String, UnitUsage>,
    taken: impl IntoIterator<Item = (&'a str, u64, f64)>,
) -> HashMap<String, Carved> {
    let mut want: HashMap<&str, Carved> = HashMap::new();
    for (unit, mem, cpu) in taken {
        let e = want.entry(unit).or_default();
        e.mem += mem;
        e.cpu += cpu;
        e.procs += 1;
    }
    let mut out = HashMap::with_capacity(want.len());
    for (name, w) in want {
        let Some(u) = units.get_mut(name) else {
            continue;
        };
        let got = Carved {
            mem: w.mem.min(u.mem.mem),
            cpu: w.cpu.min(u.cpu),
            procs: w.procs,
        };
        u.mem.mem -= got.mem;
        u.cpu -= got.cpu;
        out.insert(name.to_string(), got);
    }
    out
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// `Todoist.AppImag` (comm is cut at 15 chars) -> the `todoist` app.
fn match_by_name(comm: &str, apps: &[&Arc<UnitRef>]) -> Option<Arc<UnitRef>> {
    let c = norm(comm);
    if c.len() < 4 {
        return None;
    }
    apps.iter()
        .find(|u| {
            let id = norm(u.id.rsplit('.').next().unwrap_or(&u.id));
            id.len() >= 4 && (c.starts_with(&id) || id.starts_with(&c))
        })
        .map(|u| (*u).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, ppid: i32, comm: &str, start: u64) -> Proc {
        Proc {
            pid,
            ppid,
            comm: comm.into(),
            state: 'S',
            start,
            ticks: 0,
            rss: 0,
            cpu: 0.0,
        }
    }

    fn unit(kind: &'static str, name: &str, id: &str) -> Arc<UnitRef> {
        Arc::new(UnitRef::new(kind, name.into(), id.into()))
    }

    fn usage(name: &str, mem: u64, cpu: f64) -> (String, UnitUsage) {
        (
            name.into(),
            UnitUsage {
                name: name.into(),
                id: name.into(),
                kind: "app",
                mem: crate::cgroup::MemStat {
                    mem,
                    ..Default::default()
                },
                cpu,
            },
        )
    }

    #[test]
    fn helper_units_an_app_starts_go_to_that_app() {
        let procs: HashMap<i32, Proc> = [
            proc(3273, 1, "gnome-shell", 1),
            proc(6449, 3273, "orca-ide", 10),
            proc(6687, 6449, "orca-ide", 11),
            proc(13905, 6687, "fish", 20),
            proc(13977, 13905, "claude", 21),
            proc(7000, 6449, "brave", 30),
            proc(8000, 3273, "helperd", 40),
        ]
        .into_iter()
        .map(|p| (p.pid, p))
        .collect();
        let shell = unit(
            "session",
            "org.gnome.Shell@wayland.service",
            "org.gnome.Shell",
        );
        let orca = unit("app", "app-gnome-orca-6449.scope", "orca");
        let daemon = unit("app", "orca-daemon-7ba5.scope", "orca-daemon-7ba5");
        let ptys = unit(
            "app",
            "orca_ptys_orca_daemon_7ba5.slice",
            "orca_ptys_orca_daemon_7ba5",
        );
        let brave = unit("app", "brave.scope", "brave");
        let helper = unit("app", "helperd.scope", "helperd");
        let unit_of: HashMap<i32, Arc<UnitRef>> = [
            (3273, shell),
            (6449, orca),
            (6687, daemon),
            (13905, ptys.clone()),
            (13977, ptys),
            (7000, brave),
            (8000, helper),
        ]
        .into_iter()
        .collect();
        let got = adopted(&procs, &unit_of, &|id| id == "brave");
        let want: HashMap<String, String> = [
            ("orca-daemon-7ba5.scope", "app-gnome-orca-6449.scope"),
            (
                "orca_ptys_orca_daemon_7ba5.slice",
                "app-gnome-orca-6449.scope",
            ),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn carve_takes_claude_out_of_its_app() {
        let mut units: HashMap<String, UnitUsage> = [
            usage("warp", 8000, 900.0),
            usage("term", 100, 5.0),
            usage("brave", 500, 10.0),
        ]
        .into_iter()
        .collect();
        let got = carve(
            &mut units,
            [
                ("warp", 3000, 200.0),
                ("warp", 2000, 70.0),
                ("term", 400, 8.0),
                ("gone", 50, 1.0),
            ],
        );
        assert_eq!(units["warp"].mem.mem, 3000);
        assert_eq!(units["warp"].cpu, 630.0);
        assert_eq!(
            got["warp"],
            Carved {
                mem: 5000,
                cpu: 270.0,
                procs: 2
            }
        );
        assert_eq!(units["term"].mem.mem, 0, "capped at what the unit holds");
        assert_eq!(units["term"].cpu, 0.0);
        assert_eq!(got["term"].mem, 100);
        assert_eq!(units["brave"].mem.mem, 500, "untouched");
        assert!(!got.contains_key("gone"), "unknown unit is skipped");
    }

    #[test]
    fn scope_shapes() {
        assert_eq!(
            scope_shape("app-gnome-codium-7128.scope"),
            (false, Some(7128))
        );
        assert_eq!(
            scope_shape("app-com.google.Chrome-11619.scope"),
            (true, Some(11619))
        );
        assert_eq!(
            scope_shape("app-com.mitchellh.ghostty.service"),
            (false, None)
        );
        assert_eq!(
            scope_shape("org.gnome.Shell@wayland.service"),
            (false, None)
        );
    }

    #[test]
    fn portal_scope_with_same_pid_is_an_alias() {
        let units = [
            UnitRef::new(
                "app",
                "app-gnome-google\\x2dchrome-500.scope".into(),
                "google-chrome".into(),
            ),
            UnitRef::new(
                "app",
                "app-com.google.Chrome-500.scope".into(),
                "com.google.Chrome".into(),
            ),
            UnitRef::new(
                "app",
                "app-org.chromium.Chromium-101.scope".into(),
                "org.chromium.Chromium".into(),
            ),
        ];
        let map = aliases(units.iter());
        assert_eq!(map.len(), 1);
        assert_eq!(
            map["app-com.google.Chrome-500.scope"],
            "app-gnome-google\\x2dchrome-500.scope"
        );
    }

    #[test]
    fn processes_go_to_the_right_app() {
        let shell = unit(
            "session",
            "org.gnome.Shell@wayland.service",
            "org.gnome.Shell",
        );
        let session_mgr = unit(
            "app",
            "app-gnome\\x2dsession\\x2dmanager.slice",
            "gnome-session-manager",
        );
        let brave = unit(
            "app",
            "app-gnome-brave\\x2dbrowser-100.scope",
            "brave-browser",
        );
        let portal = unit(
            "app",
            "app-org.chromium.Chromium-101.scope",
            "org.chromium.Chromium",
        );
        let todoist = unit("app", "app-gnome-todoist-300.scope", "todoist");
        let solaar = unit("app", "app-gnome-solaar-400.scope", "solaar");
        let procs: HashMap<i32, Proc> = [
            proc(10, 1, "systemd", 1),
            proc(20, 10, "gnome-session-b", 2),
            proc(50, 10, "gnome-shell", 3),
            proc(60, 50, "Xwayland", 4),
            proc(100, 50, "brave-browser-s", 5),
            proc(101, 100, "brave", 6),
            proc(102, 101, "brave", 7),
            proc(300, 50, "todoist", 8),
            proc(301, 10, "Todoist.AppImag", 9),
            proc(400, 20, "solaar", 10),
            proc(500, 10, "sublime_text", 11),
            proc(501, 500, "plugin_host-3.8", 12),
            proc(502, 10, "chrome_crashpad", 13),
        ]
        .into_iter()
        .map(|p| (p.pid, p))
        .collect();
        let unit_of: HashMap<i32, Arc<UnitRef>> = [
            (20, session_mgr.clone()),
            (50, shell.clone()),
            (60, shell.clone()),
            (100, brave.clone()),
            (101, portal.clone()),
            (102, shell.clone()),
            (300, todoist.clone()),
            (301, shell.clone()),
            (400, solaar.clone()),
            (500, shell.clone()),
            (501, shell.clone()),
            (502, shell.clone()),
        ]
        .into_iter()
        .collect();
        let entry_for = |comm: &str| (comm == "sublime_text").then(|| "sublime_text".to_string());
        let got = assign(&procs, &unit_of, Some(10), &entry_for);
        assert_eq!(
            got[&101], brave,
            "portal scope folds into the launcher scope above it"
        );
        assert_eq!(got[&102], brave, "launch-race leftover in the shell cgroup");
        assert_eq!(
            got[&301], todoist,
            "reparented AppImage child matched by name"
        );
        assert_eq!(got[&400], solaar, "autostart app keeps its own scope");
        assert_eq!(got[&60], shell, "shell helpers stay");
        assert_eq!(got[&50], shell, "the shell itself stays");
        assert_eq!(
            got[&500].id, "sublime_text",
            "detached app gets its own unit"
        );
        assert_eq!(got[&500].name, "detached-sublime_text");
        assert_eq!(got[&500].kind, "app");
        assert!(
            Arc::ptr_eq(&got[&500], &got[&501]),
            "its children follow it"
        );
        assert_eq!(got[&502], shell, "no launcher entry: stays with the shell");
    }
}
