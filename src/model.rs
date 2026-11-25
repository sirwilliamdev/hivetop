use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

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
    pub agents: Vec<AgentView>,
    /// Helpers of each provider outside any session, one entry per provider
    /// with at least one process.
    pub tooling: Vec<ToolingView>,
    pub orphans: Vec<OrphanView>,
    /// Largest apps opened from a launcher and largest other units (desktop
    /// session, services, autostarted tray apps), up to a cap for each.
    pub apps: Vec<AppView>,
    /// Units with memory left out of `apps` by the caps.
    pub apps_other: AppTotals,
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

/// How an agent session was started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    /// Started by a person in a terminal or an editor.
    #[default]
    Interactive,
    /// A background job (`claude --bg`), run by the provider's own daemon.
    Job,
    /// Started by another program through an SDK (plugin observers).
    Sdk,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    #[default]
    Idle,
    Busy,
    /// Stopped until a person answers: a permission dialog, a question.
    Waiting,
}

/// Tokens sent to and produced by the model, summed over a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Tokens {
    /// `input + cache_write + output`: work done for this session. Cache reads
    /// are left out because every turn reads the whole context again.
    pub total: u64,
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write += o.cache_write;
        self.cache_read += o.cache_read;
        self.total = self.input + self.cache_write + self.output;
    }
}

/// A pull request or merge request a session opened or acted on.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChangeRequest {
    pub number: u64,
    pub url: String,
    /// `github`, `gitlab` or `other`.
    pub host: String,
    /// `open`, `merged` or `closed`.
    pub state: String,
}

/// What a session produced, in six groups. All zero means it only answered.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Outputs {
    // Code
    pub files: usize,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub commits: u32,
    pub pushes: u32,
    // Change requests
    pub changes: Vec<ChangeRequest>,
    // Documents
    pub plans: u32,
    pub docs: usize,
    pub published: usize,
    // Knowledge
    pub memory: usize,
    pub notes: u32,
    pub config: usize,
    // Operations
    pub deploys: u32,
    pub releases: u32,
    pub restores: u32,
    // Tasks and messages
    pub todos: u32,
    pub issues: u32,
    pub messages: u32,
    pub records: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SubagentView {
    pub id: String,
    /// Agent type, such as `Explore` or `general-purpose`.
    pub kind: String,
    pub description: String,
    pub running: bool,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub tokens: u64,
}

/// A background job as the provider's daemon reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct JobView {
    pub id: String,
    /// The provider's own state: `working`, `blocked`, `done`, ...
    pub state: String,
    /// One line the agent wrote about what it is doing, asking or did.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentView {
    /// Stable for the life of the root process: `<pid>:<start>`.
    pub key: String,
    /// The provider's session id when known, else the key.
    pub id: String,
    /// Provider id, such as `claude`.
    pub provider: String,
    pub kind: AgentKind,
    pub pid: i32,
    /// Short name, such as `hivetop-97`.
    pub name: String,
    /// What the session is about: a name the user gave it, else the
    /// provider's own title, else `name`.
    pub title: String,
    pub project: String,
    pub cwd: String,
    pub state: AgentState,
    /// Unix seconds when `state` last changed.
    pub state_since: u64,
    /// Why the agent waits, when `state` is `waiting`.
    pub reason: Option<String>,
    pub job: Option<JobView>,
    pub model: Option<String>,
    /// Permission mode, such as `auto` or `plan`.
    pub mode: Option<String>,
    pub branch: Option<String>,
    /// Tokens in the context window after the last turn.
    pub context: u64,
    /// Session and subagents together.
    pub tokens: Tokens,
    pub outputs: Outputs,
    /// Running subagents first, then the last finished ones.
    pub subagents: Vec<SubagentView>,
    pub started_at: u64,
    pub app: Option<String>,
    pub procs: usize,
    /// Of `procs`, processes that left the session's tree but still belong to
    /// it (background commands now parented by `systemd --user`).
    pub detached: usize,
    /// RssAnon + RssShmem summed over the session's processes.
    pub rss: u64,
    pub cpu: f64,
    pub activity: Option<String>,
    pub scratchpad: Option<String>,
    pub containers: Vec<String>,
    pub container_mem: u64,
    pub container_cpu: f64,
    pub ancestors: Vec<i32>,
    /// Command that shows the session's own pane in its app (a tab of Orca),
    /// run without a shell after the window is raised. Empty when raising the
    /// window is all there is.
    pub focus: Vec<String>,
    pub spark: Vec<Point>,
    pub processes: Vec<ProcView>,
}

/// A provider's helpers outside any session (plugin workers, its daemon).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolingView {
    pub provider: String,
    pub label: String,
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

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct AppTotals {
    pub units: usize,
    pub mem: u64,
    pub cpu: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AppView {
    pub id: String,
    /// Slice the unit sits in: `app`, `session` or `background`.
    pub kind: String,
    /// Opened from a launcher: a shown desktop entry, not autostarted. The
    /// extension decides from open windows instead; this is for the CLI.
    pub ui: bool,
    pub mem: u64,
    pub cpu: f64,
    /// Agent sessions running inside this app.
    pub agents: usize,
    /// Usage of the agent processes this app hosts, already left out of
    /// `mem` and `cpu` because the AI Agents section lists them.
    pub agent_mem: u64,
    pub agent_cpu: f64,
    /// Processes owned by this app, agent ones excluded.
    pub procs: usize,
    /// Of those, processes found in another unit's cgroup (GNOME launch race, portal scopes).
    pub adopted: usize,
    /// All of them, so the extension can match open windows by PID.
    pub pids: Vec<i32>,
    pub processes: Vec<ProcView>,
}

/// An agent waiting for a person, as the `Updated` signal carries it.
#[derive(Debug, Clone, Serialize)]
pub struct WaitingView {
    pub key: String,
    pub title: String,
    pub reason: Option<String>,
    pub since: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub ts: u64,
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    pub pressure: u8,
    /// Agent sessions, SDK ones excluded.
    pub agents: usize,
    pub waiting: Vec<WaitingView>,
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
            agents: self
                .agents
                .iter()
                .filter(|a| a.kind != AgentKind::Sdk)
                .count(),
            waiting: self
                .agents
                .iter()
                .filter(|a| a.kind != AgentKind::Sdk && a.state == AgentState::Waiting)
                .map(|a| WaitingView {
                    key: a.key.clone(),
                    title: a.title.clone(),
                    reason: a.reason.clone(),
                    since: a.state_since,
                })
                .collect(),
            orphans: self.orphans.len(),
        }
    }

    /// A copy without per-process lists and sparklines.
    pub fn compact(&self) -> Snapshot {
        let mut s = self.clone();
        s.detail = false;
        s.system.spark.clear();
        for x in &mut s.tooling {
            x.processes.clear();
        }
        for x in &mut s.agents {
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
    /// Agent key -> processes (deepest first, root last) and one-off containers.
    pub agents: HashMap<String, (Vec<ProcKey>, Vec<String>)>,
    /// Background job id -> the provider binary that can stop it, and the
    /// session's processes as a fallback.
    pub jobs: HashMap<String, (PathBuf, Vec<ProcKey>)>,
    /// Ended session id -> leftover processes and containers.
    pub orphans: HashMap<String, (Vec<ProcKey>, Vec<String>)>,
    /// Containers attributed to a live session or left by an ended one.
    pub containers: HashSet<String>,
}

pub fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}
