//! Tracks what each Claude session started, so that processes and one-off
//! containers still running after the session ended can be flagged.
//!
//! Membership lives in memory only: sessions that ended before the daemon
//! started cannot be attributed.

use std::collections::{HashMap, HashSet};

use crate::procscan::{self, Proc, ProcKey};

/// Leftovers usually die within seconds of a normal exit; flag them only after this.
pub const GRACE_SECS: u64 = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    pub session_id: String,
    pub name: String,
    pub cwd: String,
    pub ended_at: u64,
}

/// A live session as the tracker needs it.
pub struct LiveSession<'a> {
    pub key: &'a str,
    pub id: &'a str,
    pub name: &'a str,
    pub cwd: &'a str,
    pub members: &'a [i32],
}

/// A live container as the tracker needs it.
pub struct LiveContainer<'a> {
    pub id: &'a str,
    pub oneoff: bool,
    pub session_key: Option<&'a str>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrphanGroup {
    pub origin: Option<Origin>,
    pub pids: Vec<i32>,
    pub containers: Vec<String>,
}

#[derive(Default)]
pub struct OrphanTracker {
    members: HashMap<ProcKey, String>,
    live: HashMap<String, (String, String, String)>,
    orphans: HashMap<ProcKey, Origin>,
    oneoff_origin: HashMap<String, String>,
    orphan_containers: HashMap<String, Origin>,
}

impl OrphanTracker {
    pub fn update(
        &mut self,
        now: u64,
        sessions: &[LiveSession],
        containers: &[LiveContainer],
        procs: &HashMap<i32, Proc>,
        children: &HashMap<i32, Vec<i32>>,
    ) -> Vec<OrphanGroup> {
        let current: HashSet<&str> = sessions.iter().map(|s| s.key).collect();
        let ended: Vec<String> = self
            .live
            .keys()
            .filter(|k| !current.contains(k.as_str()))
            .cloned()
            .collect();
        let alive = |key: &ProcKey| {
            procs
                .get(&key.0)
                .is_some_and(|p| p.start == key.1 && p.state != 'Z')
        };
        let container_ids: HashSet<&str> = containers.iter().map(|c| c.id).collect();
        for key in ended {
            let Some((id, name, cwd)) = self.live.remove(&key) else {
                continue;
            };
            let origin = Origin {
                session_id: id,
                name,
                cwd,
                ended_at: now,
            };
            for (pk, owner) in &self.members {
                if *owner == key && alive(pk) {
                    self.orphans.insert(*pk, origin.clone());
                }
            }
            for (cid, owner) in &self.oneoff_origin {
                if *owner == key && container_ids.contains(cid.as_str()) {
                    self.orphan_containers.insert(cid.clone(), origin.clone());
                }
            }
        }
        self.members.clear();
        for s in sessions {
            self.live.insert(
                s.key.to_string(),
                (s.id.to_string(), s.name.to_string(), s.cwd.to_string()),
            );
            for pid in s.members {
                if let Some(p) = procs.get(pid) {
                    self.members.insert(p.key(), s.key.to_string());
                }
            }
        }
        for c in containers {
            if let (true, Some(key)) = (c.oneoff, c.session_key) {
                self.oneoff_origin
                    .entry(c.id.to_string())
                    .or_insert_with(|| key.to_string());
            }
        }
        self.orphans.retain(|k, _| alive(k));
        self.oneoff_origin
            .retain(|cid, _| container_ids.contains(cid.as_str()));
        self.orphan_containers
            .retain(|cid, _| container_ids.contains(cid.as_str()));

        let mut groups: HashMap<String, OrphanGroup> = HashMap::new();
        for (pk, origin) in &self.orphans {
            if now.saturating_sub(origin.ended_at) < GRACE_SECS {
                continue;
            }
            let g = groups
                .entry(origin.session_id.clone())
                .or_insert_with(|| OrphanGroup {
                    origin: Some(origin.clone()),
                    ..Default::default()
                });
            g.pids.push(pk.0);
            g.pids.extend(procscan::descendants(pk.0, children));
        }
        for (cid, origin) in &self.orphan_containers {
            if now.saturating_sub(origin.ended_at) < GRACE_SECS {
                continue;
            }
            let g = groups
                .entry(origin.session_id.clone())
                .or_insert_with(|| OrphanGroup {
                    origin: Some(origin.clone()),
                    ..Default::default()
                });
            g.containers.push(cid.clone());
        }
        let mut out: Vec<OrphanGroup> = groups
            .into_values()
            .map(|mut g| {
                g.pids.sort_unstable();
                g.pids.dedup();
                g.containers.sort();
                g
            })
            .collect();
        out.sort_by_key(|g| g.origin.as_ref().map(|o| std::cmp::Reverse(o.ended_at)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, ppid: i32) -> Proc {
        Proc {
            pid,
            ppid,
            comm: "x".into(),
            state: 'S',
            start: pid as u64 * 10,
            ticks: 0,
            rss: 0,
            cpu: 0.0,
        }
    }

    fn table(list: &[(i32, i32)]) -> HashMap<i32, Proc> {
        list.iter().map(|&(p, pp)| (p, proc(p, pp))).collect()
    }

    #[test]
    fn leftover_process_becomes_orphan_after_grace() {
        let mut t = OrphanTracker::default();
        let procs = table(&[(10, 1), (11, 10), (12, 11), (13, 10)]);
        let children = procscan::children_map(&procs);
        let members = [11, 12, 13];
        let s = LiveSession {
            key: "10:100",
            id: "sid",
            name: "proj-1",
            cwd: "/p",
            members: &members,
        };
        assert!(t.update(1000, &[s], &[], &procs, &children).is_empty());

        // Root and 13 exit; 11 (a dev server) and its child 12 get reparented to 1.
        let procs = table(&[(11, 1), (12, 11)]);
        let children = procscan::children_map(&procs);
        assert!(
            t.update(1002, &[], &[], &procs, &children).is_empty(),
            "inside grace period"
        );
        let groups = t.update(1002 + GRACE_SECS, &[], &[], &procs, &children);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pids, vec![11, 12]);
        assert_eq!(groups[0].origin.as_ref().unwrap().session_id, "sid");

        let groups = t.update(1100, &[], &[], &HashMap::new(), &HashMap::new());
        assert!(groups.is_empty(), "orphans vanish once they exit");
    }

    #[test]
    fn oneoff_container_outlives_session() {
        let mut t = OrphanTracker::default();
        let procs = table(&[(10, 1)]);
        let children = procscan::children_map(&procs);
        let s = LiveSession {
            key: "10:100",
            id: "sid",
            name: "p",
            cwd: "/p",
            members: &[],
        };
        let run = LiveContainer {
            id: "abc",
            oneoff: true,
            session_key: Some("10:100"),
        };
        let stack = LiveContainer {
            id: "def",
            oneoff: false,
            session_key: Some("10:100"),
        };
        t.update(0, &[s], &[run, stack], &procs, &children);

        let after = || {
            [
                LiveContainer {
                    id: "abc",
                    oneoff: true,
                    session_key: None,
                },
                LiveContainer {
                    id: "def",
                    oneoff: false,
                    session_key: None,
                },
            ]
        };
        assert!(
            t.update(100, &[], &after(), &HashMap::new(), &HashMap::new())
                .is_empty()
        );
        let groups = t.update(
            100 + GRACE_SECS,
            &[],
            &after(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].containers, vec!["abc".to_string()]);
    }
}
