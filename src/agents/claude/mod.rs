//! Claude Code.
//!
//! A session is a `claude` process with no session above it, plus all of its
//! descendants. Claude Code writes several internal, undocumented files; each
//! one only enriches a session, which still shows without them:
//!
//! - `sessions/<pid>.json`: name, status (busy, idle, waiting and why), kind
//!   (interactive or background job). The `*.key` files next to them hold
//!   secrets and are never opened.
//! - `jobs/<id>/state.json`: a background job's state and its one-line
//!   status. Only those fields are read; the file also holds environment
//!   variables, which are skipped.
//! - `projects/<dir>/<session id>.jsonl`: the transcript, see [`transcript`].
//! - `<tmp>/<project>/<session id>/scratchpad`: the session's scratch folder.
//!
//! `claude --bg` runs jobs under a supervisor (`claude daemon run`) and one
//! terminal host per job (`claude bg-pty-host`); both are tooling, so each job
//! is its own session. The daemon's `roster.json` holds auth tokens and is
//! never read.

pub mod outputs;
pub mod subagents;
pub mod transcript;

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use self::outputs::OutputsAcc;
use self::subagents::Subagents;
use self::transcript::{Transcript, locate, mtime_secs};
use super::{AgentDetails, AgentMeta, Provider, Role};
use crate::config::ClaudeConfig;
use crate::model::{AgentKind, AgentState, JobView};
use crate::procscan::Proc;

/// How often a missing transcript or scratchpad is looked for again.
const LOOKUP_EVERY: Duration = Duration::from_secs(30);

/// Where Claude Code keeps its files.
#[derive(Debug, Clone, PartialEq)]
pub struct Paths {
    pub sessions: PathBuf,
    pub projects: PathBuf,
    pub jobs: PathBuf,
    /// Per-session temp root, holding `<project>/<session id>/scratchpad`.
    pub tmp: PathBuf,
}

impl Paths {
    /// `HIVETOP_CLAUDE_SESSIONS` and `HIVETOP_CLAUDE_TMP` (tests) win over the
    /// config, which wins over `$CLAUDE_CONFIG_DIR` or `~/.claude`.
    pub fn resolve(cfg: &ClaudeConfig, home: &Path, uid: u32) -> Self {
        let expand = |s: &str| match s.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(s),
        };
        let env = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let sessions_env = env("HIVETOP_CLAUDE_SESSIONS");
        let dir = sessions_env
            .as_ref()
            .and_then(|s| s.parent().map(Path::to_path_buf))
            .or_else(|| (!cfg.config_dir.is_empty()).then(|| expand(&cfg.config_dir)))
            .or_else(|| env("CLAUDE_CONFIG_DIR"))
            .unwrap_or_else(|| home.join(".claude"));
        Self {
            sessions: sessions_env.unwrap_or_else(|| dir.join("sessions")),
            projects: dir.join("projects"),
            jobs: dir.join("jobs"),
            tmp: env("HIVETOP_CLAUDE_TMP")
                .or_else(|| (!cfg.tmp_dir.is_empty()).then(|| expand(&cfg.tmp_dir)))
                .unwrap_or_else(|| PathBuf::from(format!("/tmp/claude-{uid}"))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionMeta {
    pub pid: i32,
    pub session_id: String,
    pub cwd: String,
    pub name: Option<String>,
    /// `derived`, `auto` or `user` (set with `/rename`).
    pub name_source: Option<String>,
    /// `busy`, `idle` or `waiting`.
    pub status: Option<String>,
    /// Why a `waiting` session waits: `dialog open`, `input needed`, ...
    pub waiting_for: Option<String>,
    /// Milliseconds since the epoch.
    pub status_updated_at: Option<u64>,
    pub entrypoint: Option<String>,
    /// `interactive` or `bg`.
    pub kind: Option<String>,
    pub job_id: Option<String>,
    pub proc_start: Option<serde_json::Value>,
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

/// The fields of `jobs/<id>/state.json` hivetop uses. Everything else in the
/// file (environment variables among it) is skipped by the parser.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct JobState {
    /// `working`, `blocked`, `done`, `stopped`, ...
    pub state: String,
    pub detail: Option<String>,
    pub needs: Option<String>,
    pub name: Option<String>,
    pub name_source: Option<String>,
    pub output: Option<JobOutput>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct JobOutput {
    pub result: Option<String>,
}

#[derive(Default)]
struct JobCache {
    files: HashMap<String, (Option<SystemTime>, Option<JobState>, u64)>,
}

impl JobCache {
    /// The job's state and the file's mtime in Unix seconds.
    fn get(&mut self, dir: &Path, id: &str) -> Option<(JobState, u64)> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        let path = dir.join(id).join("state.json");
        let meta = fs::metadata(&path).ok()?;
        let mtime = meta.modified().ok();
        let stale = self.files.get(id).is_none_or(|(m, _, _)| *m != mtime);
        if stale {
            let state = fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<JobState>(&s).ok());
            self.files
                .insert(id.to_string(), (mtime, state, mtime_secs(&meta)));
        }
        let (_, state, secs) = self.files.get(id)?;
        state.clone().map(|s| (s, *secs))
    }
}

/// Files of one live session, keyed by the session's process key.
struct SessionFiles {
    session_id: String,
    transcript: Option<Transcript>,
    subagents: Option<Subagents>,
    looked_up: Option<Instant>,
    scratchpad: Option<String>,
    scratch_checked: Option<Instant>,
}

impl SessionFiles {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            transcript: None,
            subagents: None,
            looked_up: None,
            scratchpad: None,
            scratch_checked: None,
        }
    }
}

pub struct ClaudeCode {
    paths: Paths,
    metas: MetaCache,
    current: HashMap<i32, SessionMeta>,
    jobs: JobCache,
    sessions: HashMap<String, SessionFiles>,
}

impl ClaudeCode {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            metas: MetaCache::default(),
            current: HashMap::new(),
            jobs: JobCache::default(),
            sessions: HashMap::new(),
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    fn files(&mut self, key: &str, session_id: &str) -> &mut SessionFiles {
        let reset = self
            .sessions
            .get(key)
            .is_some_and(|f| f.session_id != session_id);
        if reset {
            // `/clear` starts a new conversation in the same process.
            self.sessions.remove(key);
        }
        self.sessions
            .entry(key.to_string())
            .or_insert_with(|| SessionFiles::new(session_id))
    }
}

fn state_of(status: Option<&str>) -> Option<AgentState> {
    match status? {
        "busy" => Some(AgentState::Busy),
        "idle" => Some(AgentState::Idle),
        "waiting" => Some(AgentState::Waiting),
        _ => None,
    }
}

impl Provider for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn refresh(&mut self) {
        self.current = self.metas.refresh(&self.paths.sessions);
    }

    fn is_session(&self, p: &Proc) -> bool {
        self.current.get(&p.pid).is_some_and(|m| m.matches(p))
    }

    fn classify(&self, p: &Proc, cmdline: &str) -> Role {
        let mut args = cmdline.split(' ');
        let exe = args.next().unwrap_or("");
        let base = exe.rsplit('/').next().unwrap_or(exe);
        let claude_binary = base == "claude" || exe.contains("/claude/versions/");
        if claude_binary || p.comm == "claude" {
            if matches!(args.next(), Some("daemon" | "bg-pty-host")) {
                return Role::Host;
            }
            if p.comm == "claude" {
                return Role::Session;
            }
            // A versioned binary with no session file: a pre-started spare.
            return Role::Tooling;
        }
        if cmdline.contains("/.claude/") {
            Role::Tooling
        } else {
            Role::None
        }
    }

    fn describe(&mut self, root: &Proc) -> AgentMeta {
        let Some(m) = self
            .current
            .get(&root.pid)
            .filter(|m| m.matches(root))
            .cloned()
        else {
            return AgentMeta::default();
        };
        let state = state_of(m.status.as_deref());
        let mut a = AgentMeta {
            title: m
                .name
                .clone()
                .filter(|_| m.name_source.as_deref() == Some("user")),
            name: m.name.clone().unwrap_or_default(),
            id: m.session_id.clone(),
            cwd: Some(m.cwd.clone()).filter(|c| !c.is_empty()),
            kind: if m.kind.as_deref() == Some("bg") || m.job_id.is_some() {
                AgentKind::Job
            } else if m.entrypoint.as_deref() == Some("sdk-cli") {
                AgentKind::Sdk
            } else {
                AgentKind::Interactive
            },
            reason: m
                .waiting_for
                .clone()
                .filter(|_| state == Some(AgentState::Waiting)),
            state,
            state_since: m.status_updated_at.map(|ms| ms / 1000),
            started_at: m.started_at.map(|ms| ms / 1000),
            job: None,
        };
        if let Some(id) = m.job_id.as_deref()
            && let Some((job, mtime)) = self.jobs.get(&self.paths.jobs, id)
        {
            if job.state == "blocked" {
                if a.state != Some(AgentState::Waiting) {
                    a.state_since = Some(mtime);
                }
                a.state = Some(AgentState::Waiting);
                a.reason = job.detail.clone().or_else(|| job.needs.clone());
            }
            if job.name_source.as_deref() == Some("user") {
                a.title = job.name.clone();
            }
            if a.name.is_empty() {
                a.name = job.name.clone().unwrap_or_default();
            }
            a.job = Some(JobView {
                id: id.to_string(),
                detail: job
                    .detail
                    .clone()
                    .or_else(|| job.output.as_ref().and_then(|o| o.result.clone())),
                state: job.state,
            });
        }
        a
    }

    fn activity(&self, cmdline: &str) -> Option<String> {
        bash_tool_command(cmdline)
    }

    fn details(&mut self, key: &str, meta: &AgentMeta, now: u64) -> AgentDetails {
        if meta.id.is_empty() {
            return AgentDetails::default();
        }
        let projects = self.paths.projects.clone();
        let f = self.files(key, &meta.id);
        if f.transcript.is_none() && f.looked_up.is_none_or(|t| t.elapsed() >= LOOKUP_EVERY) {
            f.looked_up = Some(Instant::now());
            if let Some(path) = locate(&projects, &meta.id) {
                f.subagents = Some(Subagents::new(&path));
                f.transcript = Some(Transcript::new(path));
            }
        }
        let Some(t) = f.transcript.as_mut() else {
            return AgentDetails::default();
        };
        let grew = t.update();
        if let Some(s) = f.subagents.as_mut() {
            s.update(grew, &t.stats.agents_done, now);
        }
        let st = &t.stats;
        let mut tokens = st.tokens;
        let mut parts: Vec<&OutputsAcc> = vec![&st.outputs];
        let mut subagents = Vec::new();
        if let Some(s) = f.subagents.as_ref() {
            tokens.add(&s.tokens());
            parts.extend(s.all().map(|a| &a.transcript.stats.outputs));
            subagents = s.views(&st.agents_done, now);
        }
        AgentDetails {
            title: st.custom_title.clone().or_else(|| st.ai_title.clone()),
            model: st.model.clone(),
            mode: st.mode.clone(),
            branch: st.branch.clone().filter(|b| b != "HEAD"),
            context: st.context,
            tokens,
            outputs: OutputsAcc::view(parts),
            subagents,
        }
    }

    /// `<tmp>/<project>/<session id>/scratchpad`, looked up again every 30 s
    /// until it exists (Claude Code creates it lazily).
    fn scratchpad(&mut self, key: &str, meta: &AgentMeta) -> Option<String> {
        let id = meta.id.as_str();
        if id.is_empty() || id.contains('/') || id.contains("..") {
            return None;
        }
        let tmp = self.paths.tmp.clone();
        let f = self.files(key, id);
        if f.scratchpad.is_some()
            || f.scratch_checked
                .is_some_and(|t| t.elapsed() < LOOKUP_EVERY)
        {
            return f.scratchpad.clone();
        }
        f.scratch_checked = Some(Instant::now());
        f.scratchpad = fs::read_dir(&tmp)
            .ok()
            .and_then(|dir| {
                dir.flatten()
                    .map(|e| e.path().join(id).join("scratchpad"))
                    .find(|p| p.is_dir())
            })
            .map(|p| p.to_string_lossy().into_owned());
        f.scratchpad.clone()
    }

    fn retain(&mut self, live: &HashSet<String>) {
        self.sessions.retain(|k, _| live.contains(k));
        let jobs: HashSet<&str> = self
            .current
            .values()
            .filter_map(|m| m.job_id.as_deref())
            .collect();
        self.jobs.files.retain(|id, _| jobs.contains(id.as_str()));
    }
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

    fn provider(dir: &Path) -> ClaudeCode {
        ClaudeCode::new(Paths {
            sessions: dir.join("sessions"),
            projects: dir.join("projects"),
            jobs: dir.join("jobs"),
            tmp: dir.join("tmp"),
        })
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hivetop-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sessions")).unwrap();
        d
    }

    #[test]
    fn roles_of_claude_processes() {
        let c = provider(Path::new("/nonexistent"));
        let role = |comm: &str, cmd: &str| c.classify(&proc(1, 0, comm, 1), cmd);
        assert_eq!(role("claude", "claude --effort xhigh"), Role::Session);
        assert_eq!(
            role(
                "claude",
                "/home/u/.local/bin/claude daemon run --origin transient"
            ),
            Role::Host
        );
        assert_eq!(
            role(
                "2.1.319",
                "claude bg-pty-host --bg-pty-host /tmp/s.sock 200 50 -- /x"
            ),
            Role::Host
        );
        assert_eq!(
            role(
                "2.1.319",
                "/home/u/.local/share/claude/versions/2.1.319 --bg-spare /tmp/c.sock"
            ),
            Role::Tooling
        );
        assert_eq!(
            role(
                "bun",
                "/home/u/.bun/bin/bun /home/u/.claude/plugins/x/worker.js"
            ),
            Role::Tooling
        );
        assert_eq!(role("python3", "python3 -m http.server"), Role::None);
    }

    #[test]
    fn meta_marks_non_claude_comm_as_session() {
        let dir = tmpdir("meta");
        fs::write(
            dir.join("sessions/400.json"),
            r#"{"pid":400,"sessionId":"abc","cwd":"/x","procStart":"77","kind":"bg","jobId":"1edda416","status":"idle"}"#,
        )
        .unwrap();
        let mut c = provider(&dir);
        c.refresh();
        assert!(c.is_session(&proc(400, 1, "2.1.319", 77)));
        assert!(!c.is_session(&proc(400, 1, "2.1.319", 99)), "reused pid");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn describe_waiting_and_jobs() {
        let dir = tmpdir("describe");
        fs::write(
            dir.join("sessions/10.json"),
            r#"{"pid":10,"sessionId":"s1","cwd":"/p","name":"p-1","nameSource":"derived","status":"waiting","waitingFor":"dialog open","statusUpdatedAt":1794137963522,"entrypoint":"cli","kind":"interactive"}"#,
        )
        .unwrap();
        fs::write(
            dir.join("sessions/20.json"),
            r#"{"pid":20,"sessionId":"s2","cwd":"/q","name":"single-word-ok-reply","nameSource":"auto","status":"idle","kind":"bg","jobId":"1edda416"}"#,
        )
        .unwrap();
        fs::write(
            dir.join("sessions/30.json"),
            r#"{"pid":30,"sessionId":"s3","cwd":"/r","name":"Release notes","nameSource":"user","status":"busy","entrypoint":"sdk-cli"}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("jobs/1edda416")).unwrap();
        fs::write(
            dir.join("jobs/1edda416/state.json"),
            r#"{"state":"blocked","detail":"Should I push main?","tokens":20,"providerEnv":{"SECRET":"x"},"name":"Design system","nameSource":"user"}"#,
        )
        .unwrap();
        let mut c = provider(&dir);
        c.refresh();
        let a = c.describe(&proc(10, 1, "claude", 1));
        assert_eq!(a.state, Some(AgentState::Waiting));
        assert_eq!(a.reason.as_deref(), Some("dialog open"));
        assert_eq!(a.state_since, Some(1_794_137_963));
        assert_eq!(a.kind, AgentKind::Interactive);
        assert_eq!(a.title, None);
        let j = c.describe(&proc(20, 1, "2.1.319", 1));
        assert_eq!(j.kind, AgentKind::Job);
        assert_eq!(j.state, Some(AgentState::Waiting));
        assert_eq!(j.reason.as_deref(), Some("Should I push main?"));
        assert_eq!(j.title.as_deref(), Some("Design system"));
        assert_eq!(j.job.as_ref().map(|x| x.state.as_str()), Some("blocked"));
        let s = c.describe(&proc(30, 1, "claude", 1));
        assert_eq!(s.kind, AgentKind::Sdk);
        assert_eq!(s.title.as_deref(), Some("Release notes"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn details_from_transcript_and_subagents() {
        let dir = tmpdir("details");
        fs::create_dir_all(dir.join("projects/-p/s1/subagents")).unwrap();
        fs::write(
            dir.join("projects/-p/s1.jsonl"),
            concat!(
                r#"{"type":"ai-title","aiTitle":"Popup layout"}"#,
                "\n",
                r#"{"type":"assistant","isSidechain":false,"gitBranch":"HEAD","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":9,"cache_creation_input_tokens":90,"cache_read_input_tokens":1000}}}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.join("projects/-p/s1/subagents/agent-a1.meta.json"),
            r#"{"agentType":"Explore","description":"Map coupling","toolUseId":"toolu_1"}"#,
        )
        .unwrap();
        fs::write(
            dir.join("projects/-p/s1/subagents/agent-a1.jsonl"),
            r#"{"type":"assistant","message":{"id":"x","usage":{"input_tokens":0,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#,
        )
        .unwrap();
        let mut c = provider(&dir);
        let meta = AgentMeta {
            id: "s1".into(),
            ..Default::default()
        };
        let d = c.details("1:1", &meta, crate::unix_now());
        assert_eq!(d.title.as_deref(), Some("Popup layout"));
        assert_eq!(d.model.as_deref(), Some("opus 5.5"));
        assert_eq!(d.branch, None, "detached HEAD is hidden");
        assert_eq!(d.context, 1091);
        assert_eq!(d.tokens.total, 100 + 10);
        assert_eq!(d.subagents.len(), 1);
        assert!(d.subagents[0].running);
        fs::remove_dir_all(&dir).unwrap();
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
        let dir = tmpdir("metacache");
        let s = dir.join("sessions");
        fs::write(
            s.join("10.json"),
            r#"{"pid":10,"sessionId":"s1","cwd":"/p","name":"p-1","status":"busy"}"#,
        )
        .unwrap();
        fs::write(s.join("10.abc.key"), "secret").unwrap();
        let mut cache = MetaCache::default();
        let metas = cache.refresh(&s);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[&10].status.as_deref(), Some("busy"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paths_follow_test_overrides() {
        let cfg = ClaudeConfig {
            config_dir: "~/cc".into(),
            ..Default::default()
        };
        let p = Paths::resolve(&cfg, Path::new("/home/u"), 1000);
        if std::env::var_os("HIVETOP_CLAUDE_SESSIONS").is_none() {
            assert_eq!(p.sessions, PathBuf::from("/home/u/cc/sessions"));
            assert_eq!(p.projects, PathBuf::from("/home/u/cc/projects"));
        }
    }
}
