//! Apps that host agent sessions in their own panes, and how to bring one
//! session's pane to the front.
//!
//! Raising the host's window is generic (the extension walks the session's
//! ancestors to a window). An app with tabs needs one more step to show the
//! right one, which only the app itself can do. The session's environment says
//! which app it runs in and which pane is its own.
//!
//! Only the variables named here are read: the environment also holds tokens.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;

/// The app a session runs in, as far as its environment tells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Host {
    /// App id of the host, for sessions whose process tree left its scope.
    pub app: Option<String>,
    /// Command that shows the session's pane in the host, run without a shell.
    pub focus: Vec<String>,
}

/// Reads the host of process `pid`, `None` when it runs in no known host.
pub fn read(proc_root: &Path, pid: i32) -> Option<Host> {
    let raw = fs::read(proc_root.join(pid.to_string()).join("environ")).ok()?;
    detect(&raw, hivetop_cli())
}

/// The `hivetop` command installed next to the running daemon.
fn hivetop_cli() -> &'static str {
    static CLI: OnceLock<String> = OnceLock::new();
    CLI.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .map(|e| e.with_file_name("hivetop"))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("hivetop"))
            .to_string_lossy()
            .into_owned()
    })
}

/// Host from a NUL-separated environment block. `hivetop` is the CLI that
/// runs the steps a host needs more than one command for.
pub fn detect(environ: &[u8], hivetop: &str) -> Option<Host> {
    orca(&Env(environ), hivetop)
}

struct Env<'a>(&'a [u8]);

impl<'a> Env<'a> {
    /// Value of `name`, `None` when unset or empty.
    fn get(&self, name: &str) -> Option<&'a str> {
        self.0.split(|&b| b == 0).find_map(|entry| {
            let (k, v) = std::str::from_utf8(entry).ok()?.split_once('=')?;
            (k == name && !v.is_empty()).then_some(v)
        })
    }
}

/// Orca runs agents as terminals and as chats. A terminal agent gets the
/// handle of its terminal, which `orca terminal switch` brings forward. Orca
/// has no command to show a chat, and a chat's environment does not name its
/// workspace, so `hivetop orca-show` asks Orca for it and opens that workspace.
fn orca(env: &Env, hivetop: &str) -> Option<Host> {
    if env.get("ORCA_APP_VERSION").is_none() && env.get("TERM_PROGRAM") != Some("Orca") {
        return None;
    }
    let cli = env
        .get("ORCA_CLI_COMMAND")
        .filter(|c| Path::new(c).is_absolute())
        .unwrap_or("orca");
    let terminal = env.get("ORCA_TERMINAL_HANDLE").filter(|h| is_token(h));
    let chat = env.get("ORCA_AGENT_SESSION_ID").filter(|s| is_token(s));
    let focus: Vec<&str> = match (terminal, chat) {
        (Some(h), _) => vec![cli, "terminal", "switch", "--terminal", h],
        (None, Some(id)) => vec![hivetop, "orca-show", cli, id],
        (None, None) => vec![],
    };
    let focus = focus.into_iter().map(str::to_string).collect();
    Some(Host {
        app: Some("orca".into()),
        focus,
    })
}

/// Opens, in Orca, the workspace of chat `session`. `cli` is Orca's command.
pub fn orca_show(cli: &str, session: &str) -> Result<(), String> {
    if !is_token(session) {
        return Err(format!("not an Orca session id: {session}"));
    }
    let out = Command::new(cli)
        .args(["agent", "show", session, "--json"])
        .output()
        .map_err(|e| format!("cannot run {cli}: {e}"))?;
    let shown: Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| String::from_utf8_lossy(&out.stderr).trim().to_string())?;
    let worktree = chat_worktree(&shown)?;
    let status = Command::new(cli)
        .args([
            "worktree",
            "activate",
            "--worktree",
            &format!("id:{worktree}"),
        ])
        .status()
        .map_err(|e| format!("cannot run {cli}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cli} worktree activate failed ({status})"))
    }
}

/// Workspace of the first place `orca agent show --json` lists. Folder
/// workspaces cannot be opened from the command line yet.
fn chat_worktree(shown: &Value) -> Result<String, String> {
    let place = shown["result"]["places"]
        .as_array()
        .and_then(|p| p.first())
        .ok_or("Orca shows this session nowhere (closed?)")?;
    let id = place["worktreeId"].as_str().unwrap_or_default();
    if id.is_empty() {
        return Err("Orca names no workspace for this session".into());
    }
    if id.starts_with("folder:") {
        return Err("Orca cannot open folder workspaces from the command line".into());
    }
    Ok(id.to_string())
}

/// An id Orca issued: letters, digits, `_` and `-`, never starting with `-`
/// so it cannot pass for an option.
fn is_token(s: &str) -> bool {
    s.len() <= 128
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const HIVETOP: &str = "/home/u/.local/bin/hivetop";

    fn host_of(vars: &[&str]) -> Option<Host> {
        detect(vars.join("\0").as_bytes(), HIVETOP)
    }

    #[test]
    fn orca_terminal_switches_to_its_tab() {
        let host = host_of(&[
            "HOME=/home/u",
            "TERM_PROGRAM=Orca",
            "ORCA_APP_VERSION=1.4.273",
            "ORCA_CLI_COMMAND=/home/u/.config/orca/linux-orca-cli-shim/orca",
            "ORCA_TERMINAL_HANDLE=term_c888a92f-a514-4a1e-af94-fcba91adf77f",
            "ORCA_AGENT_SESSION_ID=4a3a3045-707a-4e97-af93-2873dee8af1a",
            "ORCA_AGENT_HOOK_TOKEN=secret",
        ])
        .unwrap();
        assert_eq!(host.app.as_deref(), Some("orca"));
        assert_eq!(
            host.focus,
            vec![
                "/home/u/.config/orca/linux-orca-cli-shim/orca",
                "terminal",
                "switch",
                "--terminal",
                "term_c888a92f-a514-4a1e-af94-fcba91adf77f",
            ]
        );
    }

    #[test]
    fn orca_chat_opens_its_workspace_through_hivetop() {
        let host = host_of(&[
            "ORCA_APP_VERSION=1.4.273",
            "ORCA_CLI_COMMAND=/home/u/orca",
            "ORCA_AGENT_SESSION_ID=e204ce64-ecc6-481c-ad0d-e981a902d2fa",
        ])
        .unwrap();
        assert_eq!(host.app.as_deref(), Some("orca"));
        assert_eq!(
            host.focus,
            vec![
                HIVETOP,
                "orca-show",
                "/home/u/orca",
                "e204ce64-ecc6-481c-ad0d-e981a902d2fa"
            ]
        );
    }

    #[test]
    fn relative_cli_and_odd_ids_are_not_trusted() {
        let host = host_of(&[
            "TERM_PROGRAM=Orca",
            "ORCA_CLI_COMMAND=bin/orca",
            "ORCA_TERMINAL_HANDLE=term_1",
        ])
        .unwrap();
        assert_eq!(host.focus[0], "orca");
        let host = host_of(&[
            "TERM_PROGRAM=Orca",
            "ORCA_TERMINAL_HANDLE=a b;c",
            "ORCA_AGENT_SESSION_ID=--help",
        ])
        .unwrap();
        assert!(host.focus.is_empty());
    }

    #[test]
    fn other_terminals_have_no_host() {
        assert_eq!(host_of(&["TERM_PROGRAM=vscode", "HOME=/h"]), None);
        assert_eq!(host_of(&[]), None);
    }

    #[test]
    fn chat_worktree_from_agent_show() {
        let shown = |id: &str| serde_json::json!({"ok": true, "result": {"places": [{"kind": "chat", "worktreeId": id}]}});
        assert_eq!(
            chat_worktree(&shown("5fa2::/home/u/resonand")).as_deref(),
            Ok("5fa2::/home/u/resonand")
        );
        assert!(chat_worktree(&shown("folder:03de")).is_err());
        assert!(chat_worktree(&serde_json::json!({"result": {"places": []}})).is_err());
    }
}
