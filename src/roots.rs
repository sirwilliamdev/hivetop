use std::path::PathBuf;

/// Filesystem roots the daemon reads from. Overridable through environment
/// variables so tests can point the daemon at fixture trees. Agent providers
/// resolve their own paths (see `agents::claude::Paths`).
#[derive(Debug, Clone)]
pub struct Roots {
    pub proc: PathBuf,
    pub cgroup: PathBuf,
    pub home: PathBuf,
    pub uid: u32,
}

impl Roots {
    pub fn from_env() -> Self {
        let uid = unsafe { libc::getuid() };
        Self {
            proc: env_path("HIVETOP_PROC_ROOT", "/proc"),
            cgroup: env_path("HIVETOP_CGROUP_ROOT", "/sys/fs/cgroup"),
            home: env_path("HOME", "/"),
            uid,
        }
    }
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}
