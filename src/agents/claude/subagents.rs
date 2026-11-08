//! Subagents of a Claude Code session: `<session>/subagents/agent-<id>.meta.json`
//! (type, task description) next to `agent-<id>.jsonl` (their own transcript).
//!
//! Subagents run inside the session's process, so they have no CPU or memory
//! of their own: only what they are doing and the tokens they use.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::transcript::{Transcript, mtime_secs};
use crate::model::{SubagentView, Tokens};

/// Directory rescans when the parent transcript did not grow.
const RESCAN: Duration = Duration::from_secs(10);
/// A subagent whose transcript has not changed for this long is not running,
/// whatever the parent says (a session killed mid-task).
const STALE_SECS: u64 = 30 * 60;
/// Finished subagents shown.
const SHOW_FINISHED: usize = 3;

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Meta {
    agent_type: String,
    description: String,
    tool_use_id: String,
}

#[derive(Debug)]
pub struct Subagent {
    id: String,
    kind: String,
    description: String,
    tool_use_id: String,
    started_at: u64,
    pub transcript: Transcript,
    /// Read once more after finishing, then left alone.
    settled: bool,
}

#[derive(Debug)]
pub struct Subagents {
    dir: PathBuf,
    list: HashMap<String, Subagent>,
    scanned: Option<Instant>,
}

impl Subagents {
    /// `transcript` is the session's `<id>.jsonl`; subagents live in `<id>/subagents/`.
    pub fn new(transcript: &std::path::Path) -> Self {
        Self {
            dir: transcript.with_extension("").join("subagents"),
            list: HashMap::new(),
            scanned: None,
        }
    }

    fn running(&self, a: &Subagent, done: &HashSet<String>, now: u64) -> bool {
        !done.contains(&a.id)
            && !done.contains(&a.tool_use_id)
            && now.saturating_sub(a.transcript.mtime.max(a.started_at)) < STALE_SECS
    }

    pub fn update(&mut self, parent_grew: bool, done: &HashSet<String>, now: u64) {
        let rescan = parent_grew || self.scanned.is_none_or(|t| t.elapsed() >= RESCAN);
        if rescan {
            self.scanned = Some(Instant::now());
            if let Ok(entries) = fs::read_dir(&self.dir) {
                for e in entries.flatten() {
                    let name = e.file_name();
                    let Some(id) = name
                        .to_str()
                        .and_then(|n| n.strip_prefix("agent-"))
                        .and_then(|n| n.strip_suffix(".meta.json"))
                    else {
                        continue;
                    };
                    if self.list.contains_key(id) {
                        continue;
                    }
                    let path = e.path();
                    let meta: Meta = fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();
                    let started_at = e.metadata().map(|m| mtime_secs(&m)).unwrap_or(now);
                    self.list.insert(
                        id.to_string(),
                        Subagent {
                            id: id.to_string(),
                            kind: meta.agent_type,
                            description: meta.description,
                            tool_use_id: meta.tool_use_id,
                            started_at,
                            transcript: Transcript::new(self.dir.join(format!("agent-{id}.jsonl"))),
                            settled: false,
                        },
                    );
                }
            }
        }
        let ids: Vec<String> = self.list.keys().cloned().collect();
        for id in ids {
            let running = self.running(&self.list[&id], done, now);
            let a = self.list.get_mut(&id).expect("listed");
            if a.settled {
                continue;
            }
            a.transcript.update();
            if !running {
                a.settled = true;
            }
        }
    }

    pub fn tokens(&self) -> Tokens {
        let mut t = Tokens::default();
        for a in self.list.values() {
            t.add(&a.transcript.stats.tokens);
        }
        t
    }

    pub fn all(&self) -> impl Iterator<Item = &Subagent> {
        self.list.values()
    }

    /// Running ones first (oldest first), then the most recently finished.
    pub fn views(&self, done: &HashSet<String>, now: u64) -> Vec<SubagentView> {
        let mut running = Vec::new();
        let mut finished = Vec::new();
        for a in self.list.values() {
            let r = self.running(a, done, now);
            let v = SubagentView {
                id: a.id.clone(),
                kind: a.kind.clone(),
                description: a.description.clone(),
                running: r,
                started_at: a.started_at,
                ended_at: (!r).then_some(a.transcript.mtime.max(a.started_at)),
                tokens: a.transcript.stats.tokens.total,
            };
            if r {
                running.push(v);
            } else {
                finished.push(v);
            }
        }
        running.sort_by_key(|v| v.started_at);
        finished.sort_by_key(|v| std::cmp::Reverse(v.ended_at));
        finished.truncate(SHOW_FINISHED);
        running.extend(finished);
        running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_until_the_parent_says_done() {
        let dir = std::env::temp_dir().join(format!("hivetop-sub-{}", std::process::id()));
        let sub = dir.join("s1").join("subagents");
        fs::create_dir_all(&sub).unwrap();
        for (id, desc) in [("a1", "Map coupling"), ("a2", "Plan trait")] {
            fs::write(
                sub.join(format!("agent-{id}.meta.json")),
                format!(
                    r#"{{"agentType":"Explore","description":"{desc}","toolUseId":"toolu_{id}"}}"#
                ),
            )
            .unwrap();
            fs::write(
                sub.join(format!("agent-{id}.jsonl")),
                r#"{"type":"assistant","message":{"id":"m","usage":{"input_tokens":5,"output_tokens":7,"cache_creation_input_tokens":100,"cache_read_input_tokens":900}}}
"#,
            )
            .unwrap();
        }
        let now = crate::unix_now();
        let mut s = Subagents::new(&dir.join("s1.jsonl"));
        let done: HashSet<String> = ["toolu_a2".to_string()].into();
        s.update(true, &done, now);
        let v = s.views(&done, now);
        assert_eq!(v.len(), 2);
        assert!(v[0].running && v[0].description == "Map coupling");
        assert!(!v[1].running && v[1].ended_at.is_some());
        assert_eq!(s.tokens().total, 2 * 112);
        assert_eq!(s.tokens().cache_read, 1800);
        fs::remove_dir_all(&dir).unwrap();
    }
}
