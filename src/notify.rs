use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::config::NotifyConfig;
use crate::format;
use crate::model::{AgentKind, Snapshot};

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub key: String,
    pub summary: String,
    pub body: String,
}

#[derive(Default)]
pub struct Notifier {
    last: HashMap<String, Instant>,
    cpu_since: HashMap<String, Instant>,
    seen_orphans: HashSet<String>,
}

impl Notifier {
    pub fn evaluate(&mut self, cfg: &NotifyConfig, snap: &Snapshot, now: Instant) -> Vec<Notice> {
        let mut out = Vec::new();
        if !cfg.enabled {
            return out;
        }
        let rss_limit = cfg.session_rss_mb * 1024 * 1024;
        let mut hot = HashSet::new();
        for s in snap.agents.iter().filter(|s| s.kind != AgentKind::Sdk) {
            if s.rss > rss_limit {
                out.push(Notice {
                    key: format!("rss:{}", s.key),
                    summary: format!("AI agent {} uses {}", s.title, format::bytes(s.rss)),
                    body: session_body(s.procs, s.activity.as_deref()),
                });
            }
            if s.cpu > cfg.session_cpu_pct {
                hot.insert(s.key.clone());
                let since = *self.cpu_since.entry(s.key.clone()).or_insert(now);
                if now.duration_since(since) >= Duration::from_secs(cfg.session_cpu_secs) {
                    out.push(Notice {
                        key: format!("cpu:{}", s.key),
                        summary: format!("AI agent {} at {:.0}% CPU", s.title, s.cpu),
                        body: session_body(s.procs, s.activity.as_deref()),
                    });
                }
            }
        }
        self.cpu_since.retain(|k, _| hot.contains(k));
        if snap.system.psi.memory_some > cfg.psi_memory_some {
            out.push(Notice {
                key: "psi:memory".into(),
                summary: "System under memory pressure".into(),
                body: format!(
                    "Tasks stalled on memory {:.0}% of the last 10 s. RAM {} of {}.",
                    snap.system.psi.memory_some,
                    format::bytes(snap.system.mem_used),
                    format::bytes(snap.system.mem_total)
                ),
            });
        }
        let current: HashSet<String> = snap.orphans.iter().map(|o| o.session_id.clone()).collect();
        if cfg.orphans {
            for o in &snap.orphans {
                if self.seen_orphans.contains(&o.session_id) {
                    continue;
                }
                let mut parts = Vec::new();
                if o.procs > 0 {
                    parts.push(format!("{} process(es), {}", o.procs, format::bytes(o.rss)));
                }
                if !o.containers.is_empty() {
                    parts.push(format!("{} container(s)", o.containers.len()));
                }
                let first = o
                    .processes
                    .first()
                    .map(|p| format!(". First: {}", p.cmd))
                    .unwrap_or_default();
                out.push(Notice {
                    key: format!("orphan:{}", o.session_id),
                    summary: format!("Session {} ended and left work running", o.name),
                    body: format!("{}{first}", parts.join(", ")),
                });
            }
        }
        self.seen_orphans = current;
        let cooldown = Duration::from_secs(cfg.cooldown_secs);
        out.retain(|n| {
            let fresh = n.key.starts_with("orphan:")
                || self
                    .last
                    .get(&n.key)
                    .is_none_or(|t| now.duration_since(*t) >= cooldown);
            if fresh {
                self.last.insert(n.key.clone(), now);
            }
            fresh
        });
        self.last
            .retain(|_, t| now.duration_since(*t) < cooldown * 2);
        out
    }
}

fn session_body(procs: usize, activity: Option<&str>) -> String {
    match activity {
        Some(a) => format!("{procs} processes. Running: {a}"),
        None => format!("{procs} processes."),
    }
}

pub fn send(conn: &zbus::blocking::Connection, n: &Notice) {
    let hints: HashMap<&str, zbus::zvariant::Value> =
        HashMap::from([("urgency", zbus::zvariant::Value::U8(1))]);
    let actions: Vec<&str> = Vec::new();
    let res = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "hivetop",
            0u32,
            "utilities-system-monitor-symbolic",
            n.summary.as_str(),
            n.body.as_str(),
            actions,
            hints,
            -1i32,
        ),
    );
    if let Err(err) = res {
        eprintln!("hivetopd: notification failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentView, OrphanView};

    fn snap_with_session(rss: u64, cpu: f64) -> Snapshot {
        let mut s = Snapshot::default();
        s.agents.push(AgentView {
            key: "1:1".into(),
            name: "proj-1".into(),
            title: "proj-1".into(),
            rss,
            cpu,
            procs: 3,
            ..Default::default()
        });
        s
    }

    #[test]
    fn rss_alert_respects_cooldown() {
        let cfg = NotifyConfig {
            session_rss_mb: 1,
            ..NotifyConfig::default()
        };
        let mut n = Notifier::default();
        let t0 = Instant::now();
        let snap = snap_with_session(10 * 1024 * 1024, 0.0);
        assert_eq!(n.evaluate(&cfg, &snap, t0).len(), 1);
        assert!(
            n.evaluate(&cfg, &snap, t0 + Duration::from_secs(5))
                .is_empty()
        );
        assert_eq!(
            n.evaluate(&cfg, &snap, t0 + Duration::from_secs(601)).len(),
            1
        );
    }

    #[test]
    fn cpu_alert_needs_sustained_load() {
        let cfg = NotifyConfig {
            session_cpu_pct: 100.0,
            session_cpu_secs: 60,
            ..NotifyConfig::default()
        };
        let mut n = Notifier::default();
        let t0 = Instant::now();
        let hot = snap_with_session(0, 250.0);
        assert!(n.evaluate(&cfg, &hot, t0).is_empty());
        assert!(
            n.evaluate(
                &cfg,
                &snap_with_session(0, 10.0),
                t0 + Duration::from_secs(30)
            )
            .is_empty()
        );
        assert!(
            n.evaluate(&cfg, &hot, t0 + Duration::from_secs(40))
                .is_empty(),
            "streak restarted"
        );
        assert_eq!(
            n.evaluate(&cfg, &hot, t0 + Duration::from_secs(101)).len(),
            1
        );
    }

    #[test]
    fn orphan_alert_once_per_group() {
        let cfg = NotifyConfig::default();
        let mut n = Notifier::default();
        let mut snap = Snapshot::default();
        snap.orphans.push(OrphanView {
            session_id: "sid".into(),
            name: "p".into(),
            procs: 1,
            ..Default::default()
        });
        let t0 = Instant::now();
        assert_eq!(n.evaluate(&cfg, &snap, t0).len(), 1);
        assert!(
            n.evaluate(&cfg, &snap, t0 + Duration::from_secs(2000))
                .is_empty()
        );
    }
}
