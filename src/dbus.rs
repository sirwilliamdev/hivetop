use std::path::PathBuf;
use std::process::{self, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use zbus::fdo;
use zbus::interface;
use zbus::object_server::SignalEmitter;

use crate::model::{KillTargets, Snapshot};
use crate::procscan::{self, ProcKey};
use crate::{docker, history};

/// Detail mode stays on this long after the last detailed request, so a crashed
/// UI cannot keep the daemon sampling fast.
const ACTIVE_GRACE: Duration = Duration::from_secs(10);
const KILL_GRACE: Duration = Duration::from_secs(5);
const JOB_STOP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct Shared {
    pub snapshot: Snapshot,
    pub targets: KillTargets,
    pub docker_socket: String,
    pub active_until: Option<Instant>,
}

impl Shared {
    pub fn is_active(&self) -> bool {
        self.active_until.is_some_and(|t| t > Instant::now())
    }
}

/// Processes and containers a kill request resolves to, plus the Docker socket.
type Resolved = (Vec<ProcKey>, Vec<String>, String);

pub enum Command {
    /// Sample now and reply once the snapshot is published.
    Tick(mpsc::Sender<()>),
}

pub struct Service {
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<Command>,
    history_path: PathBuf,
    history: Mutex<Option<rusqlite::Connection>>,
    proc_root: PathBuf,
    uid: u32,
    own_pid: i32,
}

impl Service {
    pub fn new(
        shared: Arc<Mutex<Shared>>,
        tx: mpsc::Sender<Command>,
        history_path: PathBuf,
        proc_root: PathBuf,
        uid: u32,
    ) -> Self {
        Self {
            shared,
            tx,
            history_path,
            history: Mutex::new(None),
            proc_root,
            uid,
            own_pid: std::process::id() as i32,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn resolve(&self, kind: &str, id: &str) -> fdo::Result<Option<Resolved>> {
        let sh = self.lock();
        let socket = sh.docker_socket.clone();
        let found = match kind {
            "pid" => {
                let (pid, start) = id
                    .split_once(':')
                    .and_then(|(p, s)| Some((p.parse::<i32>().ok()?, s.parse::<u64>().ok()?)))
                    .ok_or_else(|| {
                        fdo::Error::InvalidArgs("pid target must be <pid>:<start>".into())
                    })?;
                match sh.targets.procs.get(&pid) {
                    Some((s, _)) if *s != start => None,
                    Some((_, comm)) => {
                        if pid <= 1
                            || pid == self.own_pid
                            || matches!(comm.as_str(), "gnome-shell" | "systemd" | "hivetopd")
                        {
                            return Err(fdo::Error::AccessDenied(format!(
                                "refusing to kill {comm}"
                            )));
                        }
                        Some((vec![(pid, start)], Vec::new()))
                    }
                    None => None,
                }
            }
            "agent" => sh.targets.agents.get(id).cloned(),
            "orphans" => sh.targets.orphans.get(id).cloned(),
            "container" => sh
                .targets
                .containers
                .contains(id)
                .then(|| (Vec::new(), vec![id.to_string()])),
            other => return Err(fdo::Error::InvalidArgs(format!("unknown kind {other}"))),
        };
        Ok(found.map(|(p, c)| (p, c, socket)))
    }
}

impl Service {
    /// Asks the provider to stop a background job (`<binary> stop <id>`), so it
    /// ends cleanly and keeps its conversation; terminates its processes when
    /// that fails.
    fn stop_job(&self, id: &str) -> bool {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }
        let Some((exe, keys)) = self.lock().targets.jobs.get(id).cloned() else {
            return false;
        };
        let keys: Vec<ProcKey> = keys
            .into_iter()
            .filter(|k| k.0 > 1 && k.0 != self.own_pid)
            .filter(|k| procscan::proc_uid(&self.proc_root, k.0) == Some(self.uid))
            .collect();
        let proc_root = self.proc_root.clone();
        let id = id.to_string();
        thread::spawn(move || {
            let stopped = process::Command::new(&exe)
                .arg("stop")
                .arg(&id)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
                .and_then(|mut child| {
                    let deadline = Instant::now() + JOB_STOP_TIMEOUT;
                    loop {
                        match child.try_wait() {
                            Ok(Some(status)) => return Some(status.success()),
                            Ok(None) if Instant::now() < deadline => {
                                thread::sleep(Duration::from_millis(100))
                            }
                            _ => {
                                let _ = child.kill();
                                let _ = child.wait();
                                return None;
                            }
                        }
                    }
                })
                .unwrap_or(false);
            if !stopped {
                eprintln!("hivetopd: {} stop {id} failed, terminating", exe.display());
                terminate(proc_root, keys);
            }
        });
        true
    }
}

#[interface(name = "io.github.sirwilliamdev.Hivetop")]
impl Service {
    /// The latest snapshot as JSON. `detail` adds process lists and sparklines and
    /// keeps the daemon in fast sampling mode for a few more seconds.
    fn get_snapshot(&self, detail: bool) -> String {
        let mut sh = self.lock();
        let out = if detail {
            sh.active_until = Some(Instant::now() + ACTIVE_GRACE);
            serde_json::to_string(&sh.snapshot)
        } else {
            serde_json::to_string(&sh.snapshot.compact())
        };
        out.unwrap_or_else(|_| "{}".into())
    }

    /// Switches detail mode. Opening samples immediately and returns once the
    /// detailed snapshot is ready.
    fn set_active(&self, open: bool) {
        self.lock().active_until = open.then(|| Instant::now() + ACTIVE_GRACE);
        if open {
            let (reply, wait) = mpsc::channel();
            if self.tx.send(Command::Tick(reply)).is_ok() {
                let _ = wait.recv_timeout(Duration::from_secs(2));
            }
        }
    }

    /// History for `range` (1h, 24h, 7d, 30d) and `target` (`kind:key`, empty for the system).
    fn get_history(&self, range: String, target: String) -> String {
        let mut guard = self.history.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = history::open_connection(&self.history_path).ok();
        }
        let Some(conn) = guard.as_ref() else {
            return json!({ "error": "history database unavailable" }).to_string();
        };
        match history::query(conn, &range, &target, crate::unix_now()) {
            Ok(v) => v.to_string(),
            Err(e) => json!({ "error": e.to_string() }).to_string(),
        }
    }

    /// Terminates a target shown in the latest snapshot. `kind` is `pid` (id
    /// `<pid>:<start>`), `agent` (agent key), `job` (background job id),
    /// `orphans` (ended session id) or `container` (attributed container id).
    /// Returns false when the target is gone.
    fn kill(&self, kind: String, id: String) -> fdo::Result<bool> {
        if kind == "job" {
            return Ok(self.stop_job(&id));
        }
        let Some((procs, containers, socket)) = self.resolve(&kind, &id)? else {
            return Ok(false);
        };
        let procs: Vec<ProcKey> = procs
            .into_iter()
            .filter(|k| k.0 > 1 && k.0 != self.own_pid)
            .filter(|k| procscan::proc_uid(&self.proc_root, k.0) == Some(self.uid))
            .collect();
        terminate(self.proc_root.clone(), procs);
        if !containers.is_empty() {
            thread::spawn(move || {
                for c in containers {
                    if !docker::stop(&socket, &c) {
                        eprintln!("hivetopd: could not stop container {c}");
                    }
                }
            });
        }
        Ok(true)
    }

    #[zbus(signal)]
    pub async fn updated(emitter: &SignalEmitter<'_>, summary: &str) -> zbus::Result<()>;
}

/// SIGTERM in the given order (children before parents), SIGKILL survivors later.
fn terminate(proc_root: PathBuf, keys: Vec<ProcKey>) {
    if keys.is_empty() {
        return;
    }
    for k in &keys {
        if procscan::still_alive(&proc_root, *k) {
            unsafe { libc::kill(k.0, libc::SIGTERM) };
        }
    }
    thread::spawn(move || {
        thread::sleep(KILL_GRACE);
        for k in keys {
            if procscan::still_alive(&proc_root, k) {
                unsafe { libc::kill(k.0, libc::SIGKILL) };
            }
        }
    });
}
