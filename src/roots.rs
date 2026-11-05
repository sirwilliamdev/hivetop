use std::path::PathBuf;

/// Filesystem roots the daemon reads from. Overridable through environment
/// variables so tests can point the daemon at fixture trees.
#[derive(Debug, Clone)]
pub struct Roots {
    pub proc: PathBuf,
    pub cgroup: PathBuf,
    pub claude_sessions: PathBuf,
    /// Claude Code's per-session temp root, holding `<project>/<session id>/scratchpad`.
    pub claude_tmp: PathBuf,
    pub home: PathBuf,
    pub uid: u32,
}

impl Roots {
    pub fn from_env() -> Self {
        let uid = unsafe { libc::getuid() };
        let home = env_path("HOME", "/");
        let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        Self {
            proc: env_path("HIVETOP_PROC_ROOT", "/proc"),
            cgroup: env_path("HIVETOP_CGROUP_ROOT", "/sys/fs/cgroup"),
            claude_sessions: std::env::var_os("HIVETOP_CLAUDE_SESSIONS")
                .map(PathBuf::from)
                .unwrap_or_else(|| claude_dir.join("sessions")),
            claude_tmp: std::env::var_os("HIVETOP_CLAUDE_TMP")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("/tmp/claude-{uid}"))),
            home,
            uid,
        }
    }
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}
