//! Claude Code session discovery.
//!
//! A session is a `claude` process with no `claude` ancestor, plus all of its
//! descendants. Claude Code also writes `~/.claude/sessions/<pid>.json`; that
//! file is an internal, undocumented format, so it only enriches a session
//! (name, status, session id) and is never required.

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::procscan::Proc;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub pid: i32,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub proc_start: Option<serde_json::Value>,
    #[serde(default)]
    pub started_at: Option<u64>,
}

impl SessionMeta {
    /// True when the file describes this exact process, not an earlier one with a reused PID.
    pub fn matches(&self, p: &Proc) -> bool {
        if self.pid != p.pid {
            return false;
        }
        let start = match &self.proc_start {
            Some(serde_json::Value::String(s)) => s.parse::<u64>().ok(),
            Some(serde_json::Value::Number(n)) => n.as_u64(),
            _ => None,
        };
        start.is_none_or(|s| s == p.start)
    }
}

/// Re-reads session files only when their mtime changes. Only `*.json` files are
/// opened: the `*.key` files next to them hold secrets.
#[derive(Default)]
pub struct MetaCache {
    files: HashMap<PathBuf, (Option<SystemTime>, Option<SessionMeta>)>,
}

impl MetaCache {
    pub fn refresh(&mut self, dir: &Path) -> HashMap<i32, SessionMeta> {
        let mut seen = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let mtime = e.metadata().and_then(|m| m.modified()).ok();
                let stale = self.files.get(&path).is_none_or(|(m, _)| *m != mtime);
                if stale {
                    let meta = fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| serde_json::from_str::<SessionMeta>(&s).ok());
                    self.files.insert(path.clone(), (mtime, meta));
                }
                seen.push(path);
            }
        }
        self.files.retain(|p, _| seen.contains(p));
        self.files
            .values()
            .filter_map(|(_, m)| m.clone())
            .map(|m| (m.pid, m))
            .collect()
    }
}

pub fn is_claude(p: &Proc, metas: &HashMap<i32, SessionMeta>) -> bool {
    p.comm == "claude" || metas.get(&p.pid).is_some_and(|m| m.matches(p))
}

/// Session roots: Claude processes with no Claude ancestor. A nested `claude -p`
/// run as a tool stays part of the session that started it.
pub fn find_roots(procs: &HashMap<i32, Proc>, metas: &HashMap<i32, SessionMeta>) -> Vec<i32> {
    let mut roots: Vec<i32> = procs
        .values()
        .filter(|p| is_claude(p, metas))
        .filter(|p| {
            let mut cur = p.ppid;
            let mut depth = 0;
            while cur > 1 && depth < 64 {
                match procs.get(&cur) {
                    Some(a) if is_claude(a, metas) => return false,
                    Some(a) => cur = a.ppid,
                    None => break,
                }
                depth += 1;
            }
            true
        })
        .map(|p| p.pid)
        .collect();
    roots.sort_unstable();
    roots
}

/// Extracts the command from Claude's Bash tool wrapper:
/// `/bin/bash -c source <snapshot> ... && eval '<command>' < /dev/null && pwd -P >| ...`
pub fn bash_tool_command(cmdline: &str) -> Option<String> {
    let start = cmdline.find("eval '")? + "eval '".len();
    let rest = &cmdline[start..];
    let end = rest.find("' < /dev/null").or_else(|| rest.rfind('\''))?;
    let cmd = rest[..end].replace("'\\''", "'");
    Some(cmd).filter(|c| !c.trim().is_empty())
}

pub fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd)
        .to_string()
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

    fn table(list: Vec<Proc>) -> HashMap<i32, Proc> {
        list.into_iter().map(|p| (p.pid, p)).collect()
    }

    #[test]
    fn roots_skip_nested_claude() {
        let procs = table(vec![
            proc(100, 1, "codium", 1),
            proc(200, 100, "claude", 2),
            proc(201, 200, "bash", 3),
            proc(202, 201, "claude", 4),
            proc(300, 1, "bun", 5),
            proc(301, 300, "claude", 6),
        ]);
        assert_eq!(find_roots(&procs, &HashMap::new()), vec![200, 301]);
    }

    #[test]
    fn meta_marks_non_claude_comm_as_root() {
        let procs = table(vec![proc(400, 1, "node", 77)]);
        let meta: SessionMeta =
            serde_json::from_str(r#"{"pid":400,"sessionId":"abc","cwd":"/x","procStart":"77"}"#)
                .unwrap();
        let metas = HashMap::from([(400, meta.clone())]);
        assert_eq!(find_roots(&procs, &metas), vec![400]);
        let reused = proc(400, 1, "node", 99);
        assert!(!meta.matches(&reused));
    }

    #[test]
    fn bash_wrapper_extraction() {
        let cmd = "/bin/bash -c source /home/u/.claude/shell-snapshots/s.sh 2>/dev/null || true && eval 'cargo test 2>&1 | tail -5' < /dev/null && pwd -P >| /tmp/claude-1-cwd";
        assert_eq!(
            bash_tool_command(cmd).as_deref(),
            Some("cargo test 2>&1 | tail -5")
        );
        let quoted = "bash -c eval 'echo '\\''hi'\\''' < /dev/null";
        assert_eq!(bash_tool_command(quoted).as_deref(), Some("echo 'hi'"));
        assert_eq!(bash_tool_command("python3 -m http.server"), None);
    }

    #[test]
    fn meta_cache_reads_only_json() {
        let dir = std::env::temp_dir().join(format!("hivetop-meta-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("10.json"),
            r#"{"pid":10,"sessionId":"s1","cwd":"/p","name":"p-1","status":"busy"}"#,
        )
        .unwrap();
        fs::write(dir.join("10.abc.key"), "secret").unwrap();
        let mut cache = MetaCache::default();
        let metas = cache.refresh(&dir);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[&10].status.as_deref(), Some("busy"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
