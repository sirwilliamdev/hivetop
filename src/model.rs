use serde::Serialize;
use std::collections::{HashMap, HashSet};

use crate::docker::Container;
use crate::procscan::ProcKey;
use crate::system::Psi;

/// A sparkline point: `[unix seconds, cpu, memory]`.
pub type Point = [f64; 3];

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub ts: u64,
    pub ncpu: usize,
    pub detail: bool,
    pub system: SystemView,
    pub sessions: Vec<SessionView>,
    pub tooling: GroupView,
    pub orphans: Vec<OrphanView>,
    pub apps: Vec<AppView>,
    pub containers: Vec<Container>,
    pub docker_ok: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemView {
    /// 0-100 across all cores.
    pub cpu: f64,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub psi: Psi,
    pub pressure: u8,
    /// Points hold cpu % and memory used %.
    pub spark: Vec<Point>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProcView {
    pub pid: i32,
    pub start: u64,
    pub comm: String,
    pub cmd: String,
    pub rss: u64,
    pub cpu: f64,
    pub infra: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionView {
    /// Stable for the life of the root process: `<pid>:<start>`.
    pub key: String,
    /// Claude session id when known, else the key.
    pub id: String,
    pub pid: i32,
    pub name: String,
    pub project: String,
    pub cwd: String,
    pub status: String,
    pub entrypoint: String,
    pub background: bool,
    pub started_at: u64,
    pub app: Option<String>,
    pub procs: usize,
    pub rss: u64,
    pub cpu: f64,
    pub activity: Option<String>,
    pub scratchpad: Option<String>,
    pub containers: Vec<String>,
    pub container_mem: u64,
    pub container_cpu: f64,
    pub ancestors: Vec<i32>,
    pub spark: Vec<Point>,
    pub processes: Vec<ProcView>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GroupView {
    pub procs: usize,
    pub rss: u64,
    pub cpu: f64,
    pub processes: Vec<ProcView>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OrphanView {
    pub session_id: String,
    pub name: String,
    pub cwd: String,
    pub ended_at: u64,
    pub procs: usize,
    pub rss: u64,
    pub cpu: f64,
    pub containers: Vec<String>,
    pub processes: Vec<ProcView>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AppView {
    pub id: String,
    pub kind: String,
    pub mem: u64,
    pub cpu: f64,
    pub sessions: usize,
    /// Processes owned by this app.
    pub procs: usize,
    /// Of those, processes found in another unit's cgroup (GNOME launch race, portal scopes).
    pub adopted: usize,
    pub processes: Vec<ProcView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub ts: u64,
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    pub pressure: u8,
    pub sessions: usize,
    pub orphans: usize,
}

impl Snapshot {
    pub fn summary(&self) -> Summary {
        Summary {
            ts: self.ts,
            cpu: self.system.cpu,
            mem_used: self.system.mem_used,
            mem_total: self.system.mem_total,
            pressure: self.system.pressure,
            sessions: self.sessions.iter().filter(|s| !s.background).count(),
            orphans: self.orphans.len(),
        }
    }

    /// A copy without per-process lists and sparklines.
    pub fn compact(&self) -> Snapshot {
        let mut s = self.clone();
        s.detail = false;
        s.system.spark.clear();
        s.tooling.processes.clear();
        for x in &mut s.sessions {
            x.spark.clear();
            x.processes.clear();
        }
        for x in &mut s.orphans {
            x.processes.clear();
        }
        for x in &mut s.apps {
            x.processes.clear();
        }
        s
    }
}

/// What the D-Bus `Kill` method may act on. Rebuilt every tick from what the
/// sampler saw, so callers can only name targets that are on screen.
#[derive(Debug, Clone, Default)]
pub struct KillTargets {
    pub procs: HashMap<i32, (u64, String)>,
    /// Session key -> processes (deepest first, root last) and one-off containers.
    pub sessions: HashMap<String, (Vec<ProcKey>, Vec<String>)>,
    /// Ended session id -> leftover processes and containers.
    pub orphans: HashMap<String, (Vec<ProcKey>, Vec<String>)>,
    /// Containers attributed to a live session or left by an ended one.
    pub containers: HashSet<String>,
}

pub fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}
