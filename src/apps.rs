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

use std::collections::HashMap;
use std::sync::Arc;

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

/// Unit each process is accounted to, keyed by PID. `unit_of` must already
/// have aliases resolved.
pub fn assign(
    procs: &HashMap<i32, Proc>,
    unit_of: &HashMap<i32, Arc<UnitRef>>,
    user_manager: Option<i32>,
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
    let mut out = HashMap::with_capacity(unit_of.len());
    for (pid, own) in unit_of {
        let target = if own.kind == "app" {
            lift(*pid, own.clone())
        } else if let Some((ancestor, unit)) = nearest(*pid, &|u| u.kind == "app") {
            lift(ancestor, unit)
        } else {
            let reparented_helper = procs.get(pid).is_some_and(|p| Some(p.ppid) == user_manager)
                && main_of
                    .get(own.name.as_str())
                    .is_some_and(|(_, main)| main != pid);
            reparented_helper
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
        ]
        .into_iter()
        .collect();
        let got = assign(&procs, &unit_of, Some(10));
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
    }
}
