use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub interval_active_ms: u64,
    pub interval_idle_ms: u64,
    pub history: HistoryConfig,
    pub notify: NotifyConfig,
    pub docker: DockerConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub rollup_secs: u64,
    pub retention_days: u64,
    pub downsample_after_days: u64,
    pub top_apps: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub enabled: bool,
    pub session_rss_mb: u64,
    pub session_cpu_pct: f64,
    pub session_cpu_secs: u64,
    pub psi_memory_some: f64,
    pub orphans: bool,
    pub cooldown_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DockerConfig {
    pub enabled: bool,
    pub socket: String,
    pub refresh_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_active_ms: 2000,
            interval_idle_ms: 5000,
            history: HistoryConfig::default(),
            notify: NotifyConfig::default(),
            docker: DockerConfig::default(),
        }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rollup_secs: 60,
            retention_days: 30,
            downsample_after_days: 7,
            top_apps: 5,
        }
    }
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            session_rss_mb: 4096,
            session_cpu_pct: 300.0,
            session_cpu_secs: 60,
            psi_memory_some: 20.0,
            orphans: true,
            cooldown_secs: 600,
        }
    }
}

impl Default for DockerConfig {
    fn default() -> Self {
        let socket = std::env::var("DOCKER_HOST")
            .ok()
            .and_then(|h| h.strip_prefix("unix://").map(str::to_string))
            .unwrap_or_else(|| "/var/run/docker.sock".to_string());
        Self {
            enabled: true,
            socket,
            refresh_secs: 30,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> (Self, Option<SystemTime>) {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let Ok(text) = std::fs::read_to_string(path) else {
            return (Self::default(), mtime);
        };
        match toml::from_str::<Config>(&text) {
            Ok(cfg) => (cfg.sanitized(), mtime),
            Err(err) => {
                eprintln!("hivetopd: ignoring invalid {}: {err}", path.display());
                (Self::default(), mtime)
            }
        }
    }

    fn sanitized(mut self) -> Self {
        self.interval_active_ms = self.interval_active_ms.clamp(500, 60_000);
        self.interval_idle_ms = self.interval_idle_ms.clamp(1000, 300_000);
        self.history.rollup_secs = self.history.rollup_secs.clamp(10, 3600);
        self.history.retention_days = self.history.retention_days.clamp(1, 366);
        self.history.downsample_after_days = self
            .history
            .downsample_after_days
            .clamp(1, self.history.retention_days);
        self.docker.refresh_secs = self.docker.refresh_secs.clamp(5, 3600);
        self
    }
}

pub fn config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("hivetop/config.toml")
}

pub fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join("hivetop")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_config_keeps_defaults() {
        let cfg: Config = toml::from_str("[notify]\nsession_rss_mb = 2048\n").unwrap();
        assert_eq!(cfg.notify.session_rss_mb, 2048);
        assert_eq!(cfg.notify.cooldown_secs, 600);
        assert_eq!(cfg.history.retention_days, 30);
    }

    #[test]
    fn sanitize_clamps_intervals() {
        let cfg: Config = toml::from_str("interval_active_ms = 1\n").unwrap();
        assert_eq!(cfg.sanitized().interval_active_ms, 500);
    }
}
