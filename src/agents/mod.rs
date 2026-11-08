//! AI coding agents: tools that run as a process tree on this machine.
//!
//! A [`Provider`] knows one tool (Claude Code today): which processes are its
//! sessions, which are its own helpers, and what it writes on disk about each
//! session. Process trees, memory, containers and orphans are generic and stay
//! in the sampler, so adding a provider means implementing this trait only.

pub mod claude;

use std::collections::{HashMap, HashSet};

use crate::model::{AgentKind, AgentState, JobView, Outputs, SubagentView, Tokens};
use crate::procscan::Proc;

/// What one process is to a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A session: a root unless another session runs above it.
    Session,
    /// A supervisor that sessions run under (Claude Code's background daemon
    /// and its terminal hosts). Never a session; counted as tooling.
    Host,
    /// A helper of the provider outside any session (plugin workers).
    Tooling,
    None,
}

/// What a provider's state files say about one session root.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentMeta {
    /// The provider's session id, empty when unknown.
    pub id: String,
    pub name: String,
    /// A title the user gave the session.
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub kind: AgentKind,
    /// `None` when the provider does not say; the sampler then guesses from activity.
    pub state: Option<AgentState>,
    pub state_since: Option<u64>,
    pub reason: Option<String>,
    pub started_at: Option<u64>,
    pub job: Option<JobView>,
}

/// What a provider learned from a session's transcript.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentDetails {
    /// The provider's own title for the session.
    pub title: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub branch: Option<String>,
    pub context: u64,
    pub tokens: Tokens,
    pub outputs: Outputs,
    pub subagents: Vec<SubagentView>,
}

pub trait Provider: Send {
    /// Short stable id, such as `claude`.
    fn id(&self) -> &'static str;
    /// Name shown to people, such as `Claude Code`.
    fn label(&self) -> &'static str;
    /// Reloads state files that changed. Called once per sample.
    fn refresh(&mut self);
    /// True when the provider's state files describe `p` as a session.
    fn is_session(&self, p: &Proc) -> bool;
    /// Role of `p` judged from its command line. Called once per process.
    fn classify(&self, p: &Proc, cmdline: &str) -> Role;
    fn describe(&mut self, root: &Proc) -> AgentMeta;
    /// The command a tool process runs, without the provider's shell wrapper.
    fn activity(&self, cmdline: &str) -> Option<String>;
    /// Details of the live session `key`; cheap when nothing changed on disk.
    fn details(&mut self, key: &str, meta: &AgentMeta, now: u64) -> AgentDetails;
    fn scratchpad(&mut self, key: &str, meta: &AgentMeta) -> Option<String>;
    /// Forgets cached state of sessions that are no longer live.
    fn retain(&mut self, live: &HashSet<String>);
}

/// Session roots: session processes with no session process above them, so a
/// nested agent run as a tool stays in the session that started it. Hosts are
/// not sessions, so a job started by a supervisor is its own root.
pub fn find_roots(procs: &HashMap<i32, Proc>, is_session: impl Fn(i32) -> bool) -> Vec<i32> {
    let mut roots: Vec<i32> = procs
        .values()
        .filter(|p| is_session(p.pid))
        .filter(|p| {
            let mut cur = p.ppid;
            for _ in 0..64 {
                if cur <= 1 {
                    break;
                }
                if is_session(cur) {
                    return false;
                }
                match procs.get(&cur) {
                    Some(a) => cur = a.ppid,
                    None => break,
                }
            }
            true
        })
        .map(|p| p.pid)
        .collect();
    roots.sort_unstable();
    roots
}

/// Last component of a working directory, used as the project name.
pub fn project_name(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd)
        .to_string()
}

/// `claude-opus-5-5` -> `opus 5.5`, `claude-haiku-4-5-20251001` -> `haiku 4.5`.
pub fn short_model(model: &str) -> Option<String> {
    let rest = model.strip_prefix("claude-").unwrap_or(model);
    let parts: Vec<&str> = rest.split('-').collect();
    let family = parts
        .iter()
        .find(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphabetic()))?;
    let version: Vec<&str> = parts
        .iter()
        .filter(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
        .copied()
        .collect();
    Some(if version.is_empty() {
        family.to_string()
    } else {
        format!("{family} {}", version.join("."))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, ppid: i32, comm: &str) -> Proc {
        Proc {
            pid,
            ppid,
            comm: comm.into(),
            state: 'S',
            start: pid as u64,
            ticks: 0,
            rss: 0,
            cpu: 0.0,
        }
    }

    #[test]
    fn nested_session_stays_in_its_parent() {
        let procs: HashMap<i32, Proc> = [
            proc(100, 1, "codium"),
            proc(200, 100, "claude"),
            proc(201, 200, "bash"),
            proc(202, 201, "claude"),
        ]
        .into_iter()
        .map(|p| (p.pid, p))
        .collect();
        let sessions: HashSet<i32> = [200, 202].into();
        assert_eq!(find_roots(&procs, |pid| sessions.contains(&pid)), vec![200]);
    }

    #[test]
    fn job_under_a_host_is_its_own_root() {
        let procs: HashMap<i32, Proc> = [
            proc(3000, 1, "systemd"),
            proc(3100, 3000, "claude"),
            proc(3200, 3100, "2.1.319"),
            proc(3300, 3200, "2.1.319"),
        ]
        .into_iter()
        .map(|p| (p.pid, p))
        .collect();
        // 3100 is the daemon (a host), 3200 its terminal host, 3300 the job.
        assert_eq!(find_roots(&procs, |pid| pid == 3300), vec![3300]);
    }

    #[test]
    fn model_names() {
        assert_eq!(short_model("claude-opus-5-5").as_deref(), Some("opus 5.5"));
        assert_eq!(
            short_model("claude-haiku-4-5-20251001").as_deref(),
            Some("haiku 4.5")
        );
        assert_eq!(
            short_model("claude-sonnet-4-20250514").as_deref(),
            Some("sonnet 4")
        );
        assert_eq!(short_model("<synthetic>"), None);
    }
}
