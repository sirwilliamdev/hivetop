//! Incremental reader for Claude Code transcripts
//! (`~/.claude/projects/<dir>/<session id>.jsonl`).
//!
//! A transcript holds the whole conversation. Only typed metadata is kept:
//! titles, model, permission mode, branch, token usage, and what tools did
//! (files edited, git operations, PR links). Message text, prompts and tool
//! input text are never stored; a shell command is matched against a few
//! patterns and dropped.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::outputs::OutputsAcc;
use crate::agents::short_model;
use crate::model::Tokens;

/// Bytes read per update at most, so a long transcript seen for the first time
/// is read over a few samples instead of stalling one.
const MAX_READ: u64 = 16 << 20;
/// Read buffer size.
const CHUNK: usize = 1 << 20;
/// A line longer than this (a huge tool result) is skipped.
const MAX_LINE: usize = 32 << 20;

/// Lines worth parsing. Everything else (API request logs, attachments, file
/// snapshots) is skipped without parsing.
const MARKERS: [&[u8]; 7] = [
    b"\"type\":\"assistant\"",
    b"\"toolUseResult\"",
    b"\"type\":\"ai-title\"",
    b"\"type\":\"custom-title\"",
    b"\"type\":\"permission-mode\"",
    b"\"type\":\"pr-link\"",
    b"<task-notification>",
];

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub ai_title: Option<String>,
    pub custom_title: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub branch: Option<String>,
    /// Tokens in context after the last main-thread turn.
    pub context: u64,
    pub tokens: Tokens,
    /// `stop_reason` of the last assistant message.
    pub stop_reason: Option<String>,
    pub outputs: OutputsAcc,
    /// Subagents (by agent id or Agent tool-use id) known to have finished.
    pub agents_done: HashSet<String>,
    last_message: Option<(String, Usage)>,
}

impl Stats {
    fn add_usage(&mut self, id: Option<&str>, u: Usage) {
        // A message is written once per content block, each copy carrying
        // the message's usage so far: the last copy replaces the earlier ones.
        if let (Some(id), Some((last_id, last))) = (id, &self.last_message)
            && id == last_id
        {
            let prev = *last;
            self.apply(&prev, -1);
        }
        self.apply(&u, 1);
        self.last_message = id.map(|i| (i.to_string(), u));
    }

    fn apply(&mut self, u: &Usage, sign: i64) {
        let f = |t: &mut u64, v: u64| {
            *t = if sign > 0 {
                t.saturating_add(v)
            } else {
                t.saturating_sub(v)
            }
        };
        f(&mut self.tokens.input, u.input_tokens);
        f(&mut self.tokens.output, u.output_tokens);
        f(&mut self.tokens.cache_write, u.cache_creation_input_tokens);
        f(&mut self.tokens.cache_read, u.cache_read_input_tokens);
        self.tokens.total = self.tokens.input + self.tokens.cache_write + self.tokens.output;
    }

    /// Feeds one transcript line.
    pub fn feed(&mut self, line: &[u8]) {
        if !MARKERS.iter().any(|m| contains(line, m)) {
            return;
        }
        if contains(line, b"<task-notification>") {
            self.task_notification(line);
            if !contains(line, b"\"toolUseResult\"") {
                return;
            }
        }
        let Ok(e) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        match e.kind.as_str() {
            "ai-title" => self.ai_title = e.ai_title.filter(|t| !t.is_empty()),
            "custom-title" => self.custom_title = e.custom_title.filter(|t| !t.is_empty()),
            "permission-mode" => self.mode = e.permission_mode,
            "pr-link" => {
                if let Some(url) = e.pr_url.as_deref() {
                    self.outputs.pr_link(url, e.pr_number.0);
                }
            }
            "assistant" => {
                let Some(m) = e.message.0 else { return };
                if let Some(u) = m.usage {
                    self.add_usage(m.id.as_deref(), u);
                    if !e.is_sidechain {
                        self.context = u.input_tokens
                            + u.cache_creation_input_tokens
                            + u.cache_read_input_tokens;
                    }
                }
                if let Some(model) = m.model.as_deref().and_then(short_model) {
                    self.model = Some(model);
                }
                if m.stop_reason.is_some() {
                    self.stop_reason = m.stop_reason;
                }
                if let Some(b) = e.git_branch.filter(|b| !b.is_empty()) {
                    self.branch = Some(b);
                }
                for b in &m.content.0 {
                    if b.kind == "tool_use" {
                        let input = b.input.0.as_ref();
                        self.outputs.tool_use(
                            b.id.as_deref(),
                            b.name.as_deref().unwrap_or(""),
                            input,
                        );
                    }
                }
            }
            "user" => {
                let Some(r) = e.tool_use_result.0 else { return };
                let tool_use_id = e
                    .message
                    .0
                    .as_ref()
                    .and_then(|m| m.content.0.iter().find_map(|b| b.tool_use_id.clone()));
                if let (Some(agent), Some(status)) = (&r.agent_id.0, &r.status.0)
                    && status != "async_launched"
                {
                    self.agents_done.insert(agent.clone());
                }
                self.outputs.tool_result(tool_use_id.as_deref(), &r);
            }
            _ => {}
        }
    }

    /// `<task-notification><task-id>ID</task-id>...<status>completed</status>`:
    /// a background subagent finished. Only the id and status are read.
    fn task_notification(&mut self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let mut rest = text.as_ref();
        while let Some(start) = rest.find("<task-notification>") {
            rest = &rest[start..];
            let end = rest.find("</task-notification>").unwrap_or(rest.len());
            let block = &rest[..end];
            let tag = |name: &str| {
                let open = format!("<{name}>");
                let close = format!("</{name}>");
                let s = block.find(&open)? + open.len();
                let e = block[s..].find(&close)? + s;
                Some(block[s..e].trim().to_string())
            };
            let finished = tag("status").is_some_and(|s| s != "running" && s != "pending");
            if finished {
                for id in [tag("task-id"), tag("tool-use-id")].into_iter().flatten() {
                    self.agents_done.insert(id);
                }
            }
            rest = &rest[end..];
            if rest.is_empty() {
                break;
            }
            rest = &rest[1..];
        }
    }
}

/// One transcript file, read from where the last update stopped.
#[derive(Debug)]
pub struct Transcript {
    pub path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    /// Inside a line too long to parse, until its end.
    skipping: bool,
    pub stats: Stats,
    /// Modification time of the file at the last update, Unix seconds.
    pub mtime: u64,
}

impl Transcript {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            partial: Vec::new(),
            skipping: false,
            stats: Stats::default(),
            mtime: 0,
        }
    }

    /// Reads what was appended since the last call. Returns true when new
    /// lines were read.
    pub fn update(&mut self) -> bool {
        let Ok(meta) = fs::metadata(&self.path) else {
            return false;
        };
        self.mtime = mtime_secs(&meta);
        let len = meta.len();
        if len < self.offset {
            // Rewritten or truncated: start over.
            self.offset = 0;
            self.partial = Vec::new();
            self.skipping = false;
            self.stats = Stats::default();
        }
        if len == self.offset {
            return false;
        }
        let Ok(mut f) = File::open(&self.path) else {
            return false;
        };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut left = (len - self.offset).min(MAX_READ);
        let mut buf = vec![0u8; CHUNK.min(left as usize)];
        let mut fed = false;
        while left > 0 {
            let want = buf.len().min(left as usize);
            let n = match f.read(&mut buf[..want]) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            left -= n as u64;
            self.offset += n as u64;
            fed |= self.split(&buf[..n]);
        }
        fed
    }

    /// Feeds the complete lines of `chunk`, keeping an unfinished last line
    /// for the next chunk. A line longer than `MAX_LINE` is skipped.
    fn split(&mut self, chunk: &[u8]) -> bool {
        let mut start = 0;
        let mut fed = false;
        while let Some(nl) = chunk[start..].iter().position(|&b| b == b'\n') {
            let end = start + nl;
            if self.skipping {
                self.skipping = false;
            } else if self.partial.is_empty() {
                self.stats.feed(&chunk[start..end]);
                fed = true;
            } else if self.partial.len() + nl <= MAX_LINE {
                self.partial.extend_from_slice(&chunk[start..end]);
                self.stats.feed(&self.partial);
                fed = true;
            }
            if !self.partial.is_empty() {
                self.partial = Vec::new();
            }
            start = end + 1;
        }
        let rest = &chunk[start..];
        if !self.skipping {
            if self.partial.len() + rest.len() <= MAX_LINE {
                self.partial.extend_from_slice(rest);
            } else {
                self.partial = Vec::new();
                self.skipping = true;
            }
        }
        fed
    }
}

pub fn mtime_secs(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Finds `<projects>/<any dir>/<session id>.jsonl`.
pub fn locate(projects: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() || session_id.contains('/') || session_id.contains("..") {
        return None;
    }
    let name = format!("{session_id}.jsonl");
    fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|d| d.path().join(&name))
        .find(|p| p.is_file())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

// Transcript line shapes. Unknown fields are skipped without being stored.

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    ai_title: Option<String>,
    custom_title: Option<String>,
    permission_mode: Option<String>,
    git_branch: Option<String>,
    is_sidechain: bool,
    pr_url: Option<String>,
    pr_number: LooseU64,
    message: Lenient<Message>,
    tool_use_result: Lenient<ToolResult>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    stop_reason: Option<String>,
    content: Blocks,
}

#[derive(Deserialize, Default, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    name: Option<String>,
    input: Lenient<ToolInput>,
    tool_use_id: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct ToolInput {
    pub command: LooseStr,
    pub file_path: LooseStr,
    pub action: LooseStr,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolResult {
    pub file_path: LooseStr,
    #[serde(rename = "type")]
    pub kind: LooseStr,
    pub structured_patch: Lenient<Vec<Hunk>>,
    /// For a file written from scratch: the number of lines written.
    pub content: LineCount,
    pub git_operation: Lenient<GitOperation>,
    pub bash_edit_diff: Lenient<BashEditDiff>,
    pub agent_id: LooseStr,
    pub status: LooseStr,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Hunk {
    pub lines: PatchLines,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct GitOperation {
    pub commit: Lenient<GitCommit>,
    pub push: Lenient<IgnoredMap>,
    pub pr: Lenient<GitPr>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct GitCommit {
    pub kind: LooseStr,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct GitPr {
    pub number: LooseU64,
    pub url: LooseStr,
    pub action: LooseStr,
}

#[derive(Deserialize, Default, Debug)]
pub struct IgnoredMap {}

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct BashEditDiff {
    pub changed_files: Vec<String>,
    pub files: Vec<BashEditFile>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct BashEditFile {
    pub hunks: Vec<Hunk>,
}

/// `Some(T)` when the value is an object that parses as `T`; `None` for any
/// other value (a string, a list, null), which is skipped.
#[derive(Debug)]
pub struct Lenient<T>(pub Option<T>);

impl<T> Default for Lenient<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Lenient<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                Ok(Lenient(
                    T::deserialize(de::value::MapAccessDeserializer::new(map)).ok(),
                ))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                Ok(Lenient(
                    T::deserialize(de::value::SeqAccessDeserializer::new(seq)).ok(),
                ))
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// A string, or `None` for any other value (skipped).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LooseStr(pub Option<String>);

impl LooseStr {
    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

/// A non-negative integer, or `None` for any other value (skipped).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct LooseU64(pub Option<u64>);

struct Scalar {
    text: Option<String>,
    number: Option<u64>,
}

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Scalar;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_str<E>(self, s: &str) -> Result<Scalar, E> {
                Ok(Scalar {
                    text: Some(s.to_string()),
                    number: s.parse().ok(),
                })
            }
            fn visit_u64<E>(self, n: u64) -> Result<Scalar, E> {
                Ok(Scalar {
                    text: None,
                    number: Some(n),
                })
            }
            fn visit_i64<E>(self, n: i64) -> Result<Scalar, E> {
                Ok(Scalar {
                    text: None,
                    number: u64::try_from(n).ok(),
                })
            }
            fn visit_f64<E>(self, _: f64) -> Result<Scalar, E> {
                Ok(Scalar::none())
            }
            fn visit_bool<E>(self, _: bool) -> Result<Scalar, E> {
                Ok(Scalar::none())
            }
            fn visit_unit<E>(self) -> Result<Scalar, E> {
                Ok(Scalar::none())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scalar, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Scalar::none())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Scalar, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Scalar::none())
            }
        }
        d.deserialize_any(V)
    }
}

impl Scalar {
    fn none() -> Self {
        Self {
            text: None,
            number: None,
        }
    }
}

impl<'de> Deserialize<'de> for LooseStr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Scalar::deserialize(d).map(|s| LooseStr(s.text))
    }
}

impl<'de> Deserialize<'de> for LooseU64 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Scalar::deserialize(d).map(|s| LooseU64(s.number))
    }
}

/// Message content: a list of blocks, or plain text that is skipped.
#[derive(Default)]
struct Blocks(Vec<Block>);

impl<'de> Deserialize<'de> for Blocks {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Lenient::<Vec<Block>>::deserialize(d).map(|l| Blocks(l.0.unwrap_or_default()))
    }
}

/// Counts of `+` and `-` lines in a diff hunk, read without keeping the text.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PatchLines {
    pub added: u64,
    pub removed: u64,
}

impl<'de> Deserialize<'de> for PatchLines {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct First(Option<char>);
        impl<'de> Deserialize<'de> for First {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl Visitor<'_> for V {
                    type Value = First;
                    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                        f.write_str("a diff line")
                    }
                    fn visit_str<E>(self, s: &str) -> Result<First, E> {
                        Ok(First(s.chars().next()))
                    }
                }
                d.deserialize_str(V)
            }
        }
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = PatchLines;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a list of diff lines")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<PatchLines, A::Error> {
                let mut out = PatchLines::default();
                while let Some(First(c)) = seq.next_element()? {
                    match c {
                        Some('+') => out.added += 1,
                        Some('-') => out.removed += 1,
                        _ => {}
                    }
                }
                Ok(out)
            }
        }
        d.deserialize_seq(V)
    }
}

/// Number of lines in a string value; 0 for anything else.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct LineCount(pub u64);

impl<'de> Deserialize<'de> for LineCount {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LineCount;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_str<E>(self, s: &str) -> Result<LineCount, E> {
                Ok(LineCount(if s.is_empty() {
                    0
                } else {
                    s.trim_end_matches('\n').lines().count() as u64
                }))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LineCount, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LineCount(0))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<LineCount, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LineCount(0))
            }
            fn visit_bool<E>(self, _: bool) -> Result<LineCount, E> {
                Ok(LineCount(0))
            }
            fn visit_i64<E>(self, _: i64) -> Result<LineCount, E> {
                Ok(LineCount(0))
            }
            fn visit_u64<E>(self, _: u64) -> Result<LineCount, E> {
                Ok(LineCount(0))
            }
            fn visit_f64<E>(self, _: f64) -> Result<LineCount, E> {
                Ok(LineCount(0))
            }
            fn visit_unit<E>(self) -> Result<LineCount, E> {
                Ok(LineCount(0))
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn feed_all(lines: &[&str]) -> Stats {
        let mut s = Stats::default();
        for l in lines {
            s.feed(l.as_bytes());
        }
        s
    }

    const ASSISTANT_A1: &str = r#"{"parentUuid":"x","isSidechain":false,"message":{"id":"m1","type":"message","model":"claude-opus-5-5","content":[{"type":"text","text":"secret words"}],"stop_reason":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":1000,"cache_read_input_tokens":50000,"output_tokens":5}},"type":"assistant","gitBranch":"main"}"#;
    const ASSISTANT_A2: &str = r#"{"isSidechain":false,"message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}],"stop_reason":"tool_use","usage":{"input_tokens":10,"cache_creation_input_tokens":1000,"cache_read_input_tokens":50000,"output_tokens":40}},"type":"assistant","gitBranch":"main"}"#;
    const ASSISTANT_B: &str = r#"{"isSidechain":false,"message":{"id":"m2","model":"claude-opus-5-5","content":[],"stop_reason":"end_turn","usage":{"input_tokens":2,"cache_creation_input_tokens":300,"cache_read_input_tokens":51000,"output_tokens":100}},"type":"assistant","gitBranch":"HEAD"}"#;

    #[test]
    fn titles_mode_and_model() {
        let s = feed_all(&[
            r#"{"type":"ai-title","aiTitle":"First idea","sessionId":"s"}"#,
            r#"{"type":"ai-title","aiTitle":"Popup layout and AI providers","sessionId":"s"}"#,
            r#"{"type":"permission-mode","permissionMode":"auto","sessionId":"s"}"#,
            r#"{"type":"user","message":{"role":"user","content":"a prompt that must not be kept"}}"#,
            ASSISTANT_A1,
        ]);
        assert_eq!(s.ai_title.as_deref(), Some("Popup layout and AI providers"));
        assert_eq!(s.mode.as_deref(), Some("auto"));
        assert_eq!(s.model.as_deref(), Some("opus 5.5"));
        assert_eq!(s.branch.as_deref(), Some("main"));
    }

    #[test]
    fn tokens_count_each_message_once() {
        let s = feed_all(&[ASSISTANT_A1, ASSISTANT_A2, ASSISTANT_B]);
        assert_eq!(s.tokens.input, 12);
        assert_eq!(s.tokens.cache_write, 1300);
        assert_eq!(s.tokens.output, 140);
        assert_eq!(s.tokens.cache_read, 101_000);
        assert_eq!(s.tokens.total, 12 + 1300 + 140);
        assert_eq!(s.context, 2 + 300 + 51_000);
        assert_eq!(s.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn sidechain_turns_do_not_set_context() {
        let side = ASSISTANT_B
            .replace("\"isSidechain\":false", "\"isSidechain\":true")
            .replace("\"m2\"", "\"m3\"");
        let s = feed_all(&[ASSISTANT_A1, &side]);
        assert_eq!(s.context, 10 + 1000 + 50_000);
    }

    #[test]
    fn subagent_completion_markers() {
        let s = feed_all(&[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"x"}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a1"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","content":"x"}]},"toolUseResult":{"status":"completed","agentId":"a2"}}"#,
            r#"{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<status>completed</status>\n<summary>words</summary>\n</task-notification>"}"#,
        ]);
        assert!(s.agents_done.contains("a1"));
        assert!(s.agents_done.contains("toolu_1"));
        assert!(s.agents_done.contains("a2"));
    }

    #[test]
    fn incremental_reads_keep_partial_lines() {
        let dir = std::env::temp_dir().join(format!("hivetop-tr-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let (head, tail) = ASSISTANT_A1.split_at(40);
        fs::write(&path, format!("{ASSISTANT_B}\n{head}")).unwrap();
        let mut t = Transcript::new(path.clone());
        assert!(t.update());
        assert_eq!(t.stats.tokens.output, 100);
        assert!(!t.update());
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{tail}").unwrap();
        assert!(t.update());
        assert_eq!(t.stats.tokens.output, 105);
        // Rewritten shorter: starts over.
        fs::write(&path, format!("{ASSISTANT_A1}\n")).unwrap();
        t.update();
        assert_eq!(t.stats.tokens.output, 5);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn locate_rejects_paths() {
        assert_eq!(locate(Path::new("/nonexistent"), "../x"), None);
        assert_eq!(locate(Path::new("/nonexistent"), ""), None);
    }
}
