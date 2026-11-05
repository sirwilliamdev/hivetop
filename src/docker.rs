//! Minimal Docker Engine client over the Unix socket. It only lists and stops
//! containers; usage numbers come from the container cgroup files.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cgroup;

#[derive(Debug, Clone, Deserialize)]
pub struct ApiContainer {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Names", default)]
    pub names: Vec<String>,
    #[serde(rename = "Image", default)]
    pub image: String,
    #[serde(rename = "Labels", default)]
    pub labels: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub project: String,
    pub service: String,
    pub working_dir: String,
    pub oneoff: bool,
    pub mem: u64,
    pub cpu: f64,
    /// Key of the live session this container is attributed to.
    pub session: Option<String>,
    /// Session id of the ended session that left this container behind.
    pub orphan_of: Option<String>,
}

impl Container {
    fn from_api(c: &ApiContainer) -> Self {
        let label = |k: &str| {
            c.labels
                .as_ref()
                .and_then(|l| l.get(k))
                .cloned()
                .unwrap_or_default()
        };
        Self {
            id: c.id.clone(),
            name: c
                .names
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_else(|| c.id.chars().take(12).collect()),
            image: c.image.clone(),
            project: label("com.docker.compose.project"),
            service: label("com.docker.compose.service"),
            working_dir: label("com.docker.compose.project.working_dir"),
            oneoff: label("com.docker.compose.oneoff").eq_ignore_ascii_case("true"),
            mem: 0,
            cpu: 0.0,
            session: None,
            orphan_of: None,
        }
    }
}

/// Sends one HTTP/1.0 request (so the reply is never chunked) and returns status and body.
pub fn request(socket: &str, method: &str, path: &str) -> io::Result<(u16, Vec<u8>)> {
    request_with_timeout(socket, method, path, Duration::from_secs(3))
}

fn request_with_timeout(
    socket: &str,
    method: &str,
    path: &str,
    timeout: Duration,
) -> io::Result<(u16, Vec<u8>)> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.0\r\nHost: docker\r\nContent-Length: 0\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    stream.take(16 * 1024 * 1024).read_to_end(&mut raw)?;
    parse_response(&raw).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad HTTP reply"))
}

fn parse_response(raw: &[u8]) -> Option<(u16, Vec<u8>)> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, raw[split + 4..].to_vec()))
}

pub fn list(socket: &str) -> io::Result<Vec<ApiContainer>> {
    let (status, body) = request(socket, "GET", "/containers/json")?;
    if status != 200 {
        return Err(io::Error::other(format!("docker replied {status}")));
    }
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn stop(socket: &str, id: &str) -> bool {
    if !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    // Docker answers only once the container stopped: up to `t` seconds of grace, then SIGKILL.
    matches!(
        request_with_timeout(
            socket,
            "POST",
            &format!("/containers/{id}/stop?t=10"),
            Duration::from_secs(20)
        ),
        Ok((204 | 304 | 404, _))
    )
}

fn container_cgroup(cgroup_root: &Path, id: &str) -> Option<PathBuf> {
    [
        cgroup_root
            .join("system.slice")
            .join(format!("docker-{id}.scope")),
        cgroup_root.join("docker").join(id),
    ]
    .into_iter()
    .find(|p| p.is_dir())
}

fn scope_names(cgroup_root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(cgroup_root.join("system.slice"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("docker-") && n.ends_with(".scope"))
                .collect()
        })
        .unwrap_or_default();
    names.sort_unstable();
    names
}

#[derive(Default)]
pub struct DockerScanner {
    pub ok: bool,
    last_list: Option<Instant>,
    last_scopes: Vec<String>,
    containers: Vec<ApiContainer>,
    prev_usage: HashMap<String, u64>,
}

impl DockerScanner {
    /// Lists containers again only when the refresh interval passed or the set of
    /// container cgroups changed; usage is read from cgroups every call.
    pub fn scan(
        &mut self,
        cgroup_root: &Path,
        socket: &str,
        refresh: Duration,
        elapsed_secs: f64,
    ) -> Vec<Container> {
        let scopes = scope_names(cgroup_root);
        let due =
            self.last_list.is_none_or(|t| t.elapsed() >= refresh) || scopes != self.last_scopes;
        if due {
            self.last_list = Some(Instant::now());
            self.last_scopes = scopes;
            match list(socket) {
                Ok(c) => {
                    self.containers = c;
                    self.ok = true;
                }
                Err(_) => {
                    self.containers.clear();
                    self.ok = false;
                }
            }
        }
        let mut next = HashMap::new();
        let out = self
            .containers
            .iter()
            .map(|api| {
                let mut c = Container::from_api(api);
                if let Some(dir) = container_cgroup(cgroup_root, &c.id) {
                    let usage = cgroup::read_cpu_usec(&dir);
                    if let Some(&prev) = self.prev_usage.get(&c.id)
                        && elapsed_secs > 0.0
                    {
                        c.cpu = usage.saturating_sub(prev) as f64 / (elapsed_secs * 1e6) * 100.0;
                    }
                    next.insert(c.id.clone(), usage);
                    c.mem = cgroup::read_mem(&dir).mem;
                }
                c
            })
            .collect();
        self.prev_usage = next;
        out
    }
}

/// Picks the session a container belongs to: a session whose cwd is the compose
/// working dir or lies inside it (addon repos, worktrees). The closest cwd wins, so
/// a worktree runtime goes to the worktree session and the main stack to the
/// session at the project root; ties go to the most recently started session.
pub fn attribute<'a>(
    working_dir: &str,
    sessions: impl Iterator<Item = (&'a str, &'a str, u64)>,
) -> Option<String> {
    if working_dir.is_empty() {
        return None;
    }
    let wd = Path::new(working_dir);
    sessions
        .filter(|(_, cwd, _)| Path::new(cwd).starts_with(wd))
        .max_by_key(|(_, cwd, started)| (std::cmp::Reverse(cwd.len()), *started))
        .map(|(key, _, _)| key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_reply() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n[{\"Id\":\"ab\"}]";
        let (status, body) = parse_response(raw).unwrap();
        assert_eq!(status, 200);
        let list: Vec<ApiContainer> = serde_json::from_slice(&body).unwrap();
        assert_eq!(list[0].id, "ab");
    }

    #[test]
    fn container_labels() {
        let api: ApiContainer = serde_json::from_str(
            r#"{"Id":"d8f4","Names":["/shop-web-run-1"],"Image":"web","Labels":{"com.docker.compose.project":"shop","com.docker.compose.project.working_dir":"/p/shop","com.docker.compose.oneoff":"True","com.docker.compose.service":"web"}}"#,
        )
        .unwrap();
        let c = Container::from_api(&api);
        assert_eq!(c.name, "shop-web-run-1");
        assert!(c.oneoff);
        assert_eq!(c.working_dir, "/p/shop");
    }

    #[test]
    fn attribution_prefers_closest_cwd() {
        let sessions = [
            ("a", "/home/u/work/shop", 10u64),
            ("b", "/home/u/work/shop/.worktrees/hotfix-1", 20),
            ("c", "/home/u/other", 30),
        ];
        let pick = |wd: &str| attribute(wd, sessions.iter().map(|(k, c, s)| (*k, *c, *s)));
        assert_eq!(pick("/home/u/work/shop").as_deref(), Some("a"));
        assert_eq!(
            pick("/home/u/work/shop/.worktrees/hotfix-1").as_deref(),
            Some("b")
        );
        assert_eq!(pick("/home/u/work").as_deref(), Some("a"));
        assert_eq!(pick("/srv/elsewhere"), None);
        assert_eq!(pick(""), None);
    }
}
