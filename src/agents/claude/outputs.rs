//! What a session produced, read from its tool calls and their results.
//!
//! Claude Code records git operations, PR links and file edits as typed data.
//! Deploys, releases, database restores and created issues are recognised from
//! the shell command; the command text is matched here and never stored.

use std::collections::{HashMap, HashSet};

use super::transcript::{Hunk, ToolInput, ToolResult};
use crate::model::{ChangeRequest, Outputs};

/// Pending shell commands are forgotten past this many (results that never come).
const MAX_PENDING: usize = 256;

#[derive(Debug, Default, Clone)]
pub struct OutputsAcc {
    code: HashSet<String>,
    docs: HashSet<String>,
    memory: HashSet<String>,
    config: HashSet<String>,
    lines_added: u64,
    lines_removed: u64,
    commits: u32,
    pushes: u32,
    changes: Vec<ChangeRequest>,
    plans: u32,
    published: HashSet<String>,
    notes: u32,
    deploys: u32,
    releases: u32,
    restores: u32,
    todos: u32,
    issues: u32,
    messages: u32,
    records: u32,
    /// Shell commands that look like `git commit` / `git push`, by tool-use
    /// id, until their result says whether Claude recorded the operation.
    pending: HashMap<String, (u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileClass {
    Code,
    Doc,
    Memory,
    Config,
    Scratch,
}

fn file_class(path: &str) -> FileClass {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    if path.starts_with("/tmp/")
        || path.starts_with("/var/tmp/")
        || path.contains("/.claude/plans/")
    {
        // Plans count through the plan tool, not as edited files.
        FileClass::Scratch
    } else if path.contains("/.claude/projects/") && path.contains("/memory/") {
        FileClass::Memory
    } else if matches!(name, "CLAUDE.md" | "CLAUDE.local.md" | "AGENTS.md")
        || path.contains("/.claude/")
    {
        FileClass::Config
    } else if matches!(ext.as_deref(), Some("md" | "mdx" | "rst" | "txt" | "adoc")) {
        FileClass::Doc
    } else {
        FileClass::Code
    }
}

/// What a shell command does, as far as outputs go.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Shell {
    commits: u32,
    pushes: u32,
    deploys: u32,
    releases: u32,
    restores: u32,
    issues: u32,
    published: u32,
}

const RUNNERS: [&str; 15] = [
    "bash", "sh", "invoke", "inv", "make", "just", "npm", "pnpm", "yarn", "fly", "flyctl",
    "vercel", "netlify", "kubectl", "helm",
];
const READERS: [&str; 12] = [
    "grep", "rg", "cat", "less", "head", "tail", "echo", "ls", "find", "sed", "awk", "printf",
];

/// The command without heredoc bodies: text fed to a program is not a command.
fn without_heredocs(command: &str) -> String {
    let mut out = String::new();
    let mut end: Option<String> = None;
    for line in command.lines() {
        if let Some(word) = &end {
            if line.trim() == word {
                end = None;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if let Some(i) = line.find("<<") {
            let word: String = line[i + 2..]
                .trim_start_matches(['-', '~'])
                .trim_start()
                .trim_start_matches(['\'', '"'])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !word.is_empty() {
                end = Some(word);
            }
        }
    }
    out
}

fn classify_shell(command: &str) -> Shell {
    let mut out = Shell::default();
    let normalized = without_heredocs(command)
        .replace("&&", "\n")
        .replace("||", "\n");
    for segment in normalized.split(['\n', ';', '|']) {
        let words: Vec<&str> = segment
            .split_whitespace()
            .skip_while(|w| w.contains('=') && !w.starts_with('-') || *w == "sudo" || *w == "time")
            .collect();
        let Some(&first) = words.first() else {
            continue;
        };
        // Only a program name or a path runs something; `('deploy',` in a
        // quoted script does not.
        if !first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~'))
        {
            continue;
        }
        let cmd = first.rsplit('/').next().unwrap_or(first);
        let args = &words[1..];
        let has = |w: &str| args.contains(&w);
        if READERS.contains(&cmd) {
            continue;
        }
        match cmd {
            "git" => {
                if has("commit") {
                    out.commits += 1;
                } else if has("push") && !has("--dry-run") {
                    out.pushes += 1;
                } else if let Some(i) = args.iter().position(|a| *a == "tag")
                    && args
                        .get(i + 1)
                        .is_some_and(|a| !a.starts_with('-') || matches!(*a, "-a" | "-s" | "-m"))
                {
                    out.releases += 1;
                }
            }
            "gh" | "glab" => match (args.first().copied(), args.get(1).copied()) {
                (Some("issue"), Some("create")) => out.issues += 1,
                (Some("release"), Some("create")) => out.releases += 1,
                _ => {}
            },
            "cargo" | "npm" | "pnpm" | "yarn" if has("publish") => out.releases += 1,
            "pandoc" | "weasyprint" | "xelatex" | "wkhtmltopdf" => out.published += 1,
            "ansible-playbook" => out.deploys += 1,
            _ => {}
        }
        let deploy_word = |w: &str| w.contains("deploy") || w.contains("update-staging");
        if deploy_word(cmd)
            || (RUNNERS.contains(&cmd)
                && args
                    .iter()
                    .filter(|a| !a.starts_with('-'))
                    .take(2)
                    .any(|a| deploy_word(a)))
        {
            out.deploys += 1;
        }
        if cmd.contains("restore")
            || words.iter().any(|w| {
                matches!(
                    *w,
                    "restore-snapshot" | "resetdb" | "dropdb" | "createdb" | "pg_restore"
                )
            })
        {
            out.restores += 1;
        }
    }
    out
}

fn change_host(url: &str) -> &'static str {
    if url.contains("/-/merge_requests/") {
        "gitlab"
    } else if url.contains("github.com/") && url.contains("/pull/") {
        "github"
    } else {
        "other"
    }
}

fn trailing_number(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

impl OutputsAcc {
    pub fn pr_link(&mut self, url: &str, number: Option<u64>) {
        self.change(url, number, None);
    }

    fn change(&mut self, url: &str, number: Option<u64>, state: Option<&str>) {
        if url.is_empty() {
            return;
        }
        let i = match self.changes.iter().position(|c| c.url == url) {
            Some(i) => i,
            None => {
                self.changes.push(ChangeRequest {
                    number: number.or_else(|| trailing_number(url)).unwrap_or(0),
                    url: url.to_string(),
                    host: change_host(url).to_string(),
                    state: "open".into(),
                });
                self.changes.len() - 1
            }
        };
        if let Some(s) = state {
            self.changes[i].state = s.to_string();
        }
    }

    /// A tool call from an assistant message.
    pub fn tool_use(&mut self, id: Option<&str>, name: &str, input: Option<&ToolInput>) {
        match name {
            "ExitPlanMode" => self.plans += 1,
            "Artifact" => {
                let publish =
                    input.is_none_or(|i| i.action.as_deref().is_none_or(|a| a == "publish"));
                if publish && let Some(path) = input.and_then(|i| i.file_path.as_deref()) {
                    self.published.insert(path.to_string());
                }
            }
            "Bash" => {
                let Some(cmd) = input.and_then(|i| i.command.as_deref()) else {
                    return;
                };
                let s = classify_shell(cmd);
                self.deploys += s.deploys;
                self.releases += s.releases;
                self.restores += s.restores;
                self.issues += s.issues;
                if s.published > 0 {
                    self.published.insert("pdf".into());
                }
                if (s.commits > 0 || s.pushes > 0)
                    && let Some(id) = id
                {
                    if self.pending.len() >= MAX_PENDING {
                        self.pending.clear();
                    }
                    self.pending.insert(id.to_string(), (s.commits, s.pushes));
                }
            }
            _ => self.mcp(name),
        }
    }

    fn mcp(&mut self, name: &str) {
        let Some(rest) = name.strip_prefix("mcp__") else {
            return;
        };
        let n = rest.to_ascii_lowercase();
        let any = |words: &[&str]| words.iter().any(|w| n.contains(w));
        if n.contains("obsidian") {
            if any(&["put_content", "append_content", "patch_content"]) {
                self.notes += 1;
            }
        } else if n.contains("todoist") {
            if any(&["add-tasks", "update-tasks", "complete-tasks"]) {
                self.todos += 1;
            }
        } else if n.contains("gmail") || n.contains("slack") || n.contains("calendar") {
            if any(&[
                "draft",
                "send",
                "reply",
                "forward",
                "create_event",
                "post_message",
            ]) {
                self.messages += 1;
            }
        } else if n.contains("claude_docs")
            || n.contains("google_docs")
            || n.contains("google_drive")
            || n.contains("google_slides")
            || n.contains("google_sheets")
        {
            if any(&["create", "batch", "update", "copy_file"]) {
                self.published.insert(n);
            }
        } else if n.contains("git__git_commit") {
            self.commits += 1;
        } else if any(&[
            "form_create",
            "form_write",
            "__confirm",
            "create_issue",
            "create_record",
        ]) {
            self.records += 1;
        }
    }

    /// The result of a tool call, from the user message that carries it.
    pub fn tool_result(&mut self, tool_use_id: Option<&str>, r: &ToolResult) {
        let pending = tool_use_id.and_then(|id| self.pending.remove(id));
        if let Some(git) = &r.git_operation.0 {
            if let Some(c) = &git.commit.0
                && c.kind.as_deref() != Some("amended")
            {
                self.commits += 1;
            }
            if git.push.0.is_some() {
                self.pushes += 1;
            }
            if let Some(pr) = &git.pr.0
                && let Some(url) = pr.url.as_deref()
            {
                let state = match pr.action.as_deref() {
                    Some("merged") => Some("merged"),
                    Some("closed") => Some("closed"),
                    _ => None,
                };
                self.change(url, pr.number.0, state);
            }
        } else if let Some((commits, pushes)) = pending {
            // Older Claude Code versions record no git operation: trust the command.
            self.commits += commits;
            self.pushes += pushes;
        }
        if let Some(path) = r.file_path.as_deref() {
            let written = matches!(r.kind.as_deref(), Some("create" | "update"));
            let patch = r.structured_patch.0.as_deref().unwrap_or(&[]);
            if written || !patch.is_empty() {
                let (mut added, removed) = hunk_lines(patch);
                if r.kind.as_deref() == Some("create") && patch.is_empty() {
                    added += r.content.0;
                }
                self.edited(path, added, removed);
            }
        }
        if let Some(diff) = &r.bash_edit_diff.0 {
            let (added, removed) = diff
                .files
                .iter()
                .map(|f| hunk_lines(&f.hunks))
                .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
            for path in &diff.changed_files {
                self.edited(path, 0, 0);
            }
            self.count_lines(added, removed);
        }
    }

    fn edited(&mut self, path: &str, added: u64, removed: u64) {
        let set = match file_class(path) {
            FileClass::Scratch => return,
            FileClass::Code => &mut self.code,
            FileClass::Doc => &mut self.docs,
            FileClass::Memory => &mut self.memory,
            FileClass::Config => &mut self.config,
        };
        set.insert(path.to_string());
        self.count_lines(added, removed);
    }

    fn count_lines(&mut self, added: u64, removed: u64) {
        self.lines_added += added;
        self.lines_removed += removed;
    }

    /// The outputs of a session and its subagents together.
    pub fn view<'a>(parts: impl IntoIterator<Item = &'a OutputsAcc>) -> Outputs {
        let mut sets: [HashSet<&str>; 5] = Default::default();
        let mut o = Outputs::default();
        for p in parts {
            for (i, s) in [&p.code, &p.docs, &p.memory, &p.config, &p.published]
                .into_iter()
                .enumerate()
            {
                sets[i].extend(s.iter().map(String::as_str));
            }
            o.lines_added += p.lines_added;
            o.lines_removed += p.lines_removed;
            o.commits += p.commits;
            o.pushes += p.pushes;
            for c in &p.changes {
                match o.changes.iter_mut().find(|x| x.url == c.url) {
                    Some(x) if c.state != "open" => x.state = c.state.clone(),
                    Some(_) => {}
                    None => o.changes.push(c.clone()),
                }
            }
            o.plans += p.plans;
            o.notes += p.notes;
            o.deploys += p.deploys;
            o.releases += p.releases;
            o.restores += p.restores;
            o.todos += p.todos;
            o.issues += p.issues;
            o.messages += p.messages;
            o.records += p.records;
        }
        o.files = sets[0].len();
        o.docs = sets[1].len();
        o.memory = sets[2].len();
        o.config = sets[3].len();
        o.published = sets[4].len();
        o
    }
}

fn hunk_lines(hunks: &[Hunk]) -> (u64, u64) {
    hunks
        .iter()
        .fold((0, 0), |a, h| (a.0 + h.lines.added, a.1 + h.lines.removed))
}

#[cfg(test)]
mod tests {
    use super::super::transcript::Stats;
    use super::*;

    fn run(lines: &[&str]) -> Outputs {
        let mut s = Stats::default();
        for l in lines {
            s.feed(l.as_bytes());
        }
        OutputsAcc::view([&s.outputs])
    }

    fn bash(id: &str, cmd: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"m-{id}","content":[{{"type":"tool_use","id":"{id}","name":"Bash","input":{{"command":{}}}}}]}}}}"#,
            serde_json::to_string(cmd).unwrap()
        )
    }

    fn result(id: &str, body: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","content":"ok"}}]}},"toolUseResult":{body}}}"#
        )
    }

    #[test]
    fn typed_git_operations() {
        let o = run(&[
            &bash("t1", "git commit -m x && git push"),
            &result(
                "t1",
                r#"{"stdout":"","gitOperation":{"commit":{"sha":"abc","kind":"committed","branch":"main"},"push":{"branch":"main"}}}"#,
            ),
            &bash("t2", "git commit --amend --no-edit"),
            &result(
                "t2",
                r#"{"gitOperation":{"commit":{"sha":"abd","kind":"amended"}}}"#,
            ),
            &result(
                "t3",
                r#"{"gitOperation":{"pr":{"number":8,"url":"https://github.com/o/r/pull/8","action":"created"}}}"#,
            ),
            &result(
                "t4",
                r#"{"gitOperation":{"pr":{"number":8,"url":"https://github.com/o/r/pull/8","action":"merged"}}}"#,
            ),
        ]);
        assert_eq!((o.commits, o.pushes), (1, 1));
        assert_eq!(o.changes.len(), 1);
        assert_eq!(o.changes[0].state, "merged");
        assert_eq!(o.changes[0].host, "github");
    }

    #[test]
    fn shell_fallback_only_without_typed_operation() {
        let o = run(&[
            &bash(
                "t1",
                "cd repo && git commit -m 'fix' && git push origin main",
            ),
            &result("t1", r#"{"stdout":"[main abc] fix"}"#),
        ]);
        assert_eq!((o.commits, o.pushes), (1, 1));
        let unanswered = run(&[&bash("t9", "git commit -m x")]);
        assert_eq!(unanswered.commits, 0);
    }

    #[test]
    fn pr_links_dedupe_and_mark_gitlab() {
        let link = r#"{"type":"pr-link","prNumber":363,"prUrl":"https://gitlab.example.io/g/r/-/merge_requests/363"}"#;
        let o = run(&[link, link]);
        assert_eq!(o.changes.len(), 1);
        assert_eq!(o.changes[0].number, 363);
        assert_eq!(o.changes[0].host, "gitlab");
        assert_eq!(o.changes[0].state, "open");
    }

    #[test]
    fn operations_from_commands() {
        let o = run(&[
            &bash("a", "scripts/deploy-production.sh"),
            &bash("b", "invoke deploy --env prod"),
            &bash("c", "docker compose run --rm db pg_restore -d app dump"),
            &bash("d", "scripts/restore.sh --devel"),
            &bash("e", "git tag v1.2.0 && git push --tags"),
            &bash("f", "gh issue create --title x"),
            &bash("g", "grep deploy README.md | head"),
            &bash("h", "git tag -l"),
            &bash("i", "pandoc a.md -o a.pdf"),
            &bash(
                "j",
                "python3 - <<'EOF'\n('deploy', x)\nscripts/restore.sh\nEOF\ncargo test",
            ),
            &bash("k", "python3 -c \"\nprint('deploy')\n\""),
        ]);
        assert_eq!(o.deploys, 2);
        assert_eq!(o.restores, 2);
        assert_eq!(o.releases, 1);
        assert_eq!(o.issues, 1);
        assert_eq!(o.published, 1);
    }

    #[test]
    fn edits_by_kind_of_file() {
        let o = run(&[
            &result(
                "e1",
                r#"{"filePath":"/repo/src/a.rs","oldString":"x","structuredPatch":[{"oldStart":1,"oldLines":2,"newStart":1,"newLines":3,"lines":[" a","-b","+c","+d"]}]}"#,
            ),
            &result(
                "e2",
                r#"{"type":"create","filePath":"/repo/README.md","content":"one\ntwo\nthree\n","structuredPatch":[]}"#,
            ),
            &result(
                "e3",
                r#"{"type":"update","filePath":"/home/u/.claude/projects/p/memory/fact.md","content":"x","structuredPatch":[{"lines":["+x"]}]}"#,
            ),
            &result(
                "e4",
                r#"{"type":"create","filePath":"/tmp/scratch/x.py","content":"a"}"#,
            ),
            &result(
                "e5",
                r#"{"stdout":"","bashEditDiff":{"changedFiles":["/repo/src/b.rs","/repo/CLAUDE.md"],"files":[{"filePath":"/repo/src/b.rs","hunks":[{"lines":["-old","+new"]}]}]}}"#,
            ),
            &result(
                "r1",
                r#"{"type":"text","file":{"filePath":"/repo/src/c.rs","content":"x"}}"#,
            ),
        ]);
        assert_eq!(o.files, 2);
        assert_eq!(o.docs, 1);
        assert_eq!(o.memory, 1);
        assert_eq!(o.config, 1);
        assert_eq!(o.lines_added, 2 + 3 + 1 + 1);
        assert_eq!(o.lines_removed, 1 + 1);
    }

    #[test]
    fn tools_by_name() {
        let call = |name: &str, input: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"x","name":"{name}","input":{input}}}]}}}}"#
            )
        };
        let o = run(&[
            &call("ExitPlanMode", "{}"),
            &call("Artifact", r#"{"file_path":"/s/page.html"}"#),
            &call("Artifact", r#"{"action":"read","url":"u"}"#),
            &call("mcp__mcp-obsidian__obsidian_append_content", "{}"),
            &call("mcp__claude_ai_Todoist__add-tasks", "{}"),
            &call("mcp__claude_ai_Gmail__create_draft", "{}"),
            &call("mcp__claude_ai_Acme_ERP__form_create", "{}"),
            &call("mcp__claude_ai_Todoist__find-tasks", "{}"),
        ]);
        assert_eq!(o.plans, 1);
        assert_eq!(o.published, 1);
        assert_eq!((o.notes, o.todos, o.messages, o.records), (1, 1, 1, 1));
    }

    #[test]
    fn nothing_produced() {
        let o = run(&[&bash("r", "ls -la && cat Cargo.toml")]);
        assert_eq!(o, Outputs::default());
    }
}
