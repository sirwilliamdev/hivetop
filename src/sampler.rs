//! One sampling pass: reads the system, groups processes into Claude sessions,
//! apps and containers, tracks orphans, feeds history and notifications, and
//! produces the snapshot the D-Bus service hands out.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::apps::{self, UnitRef};
use crate::cgroup::{self, UnitScanner};
use crate::claude::{self, MetaCache};
use crate::config::Config;
use crate::docker::{self, DockerScanner};
use crate::format;
use crate::history::History;
use crate::model::{
    AppView, GroupView, KillTargets, OrphanView, Point, ProcView, SessionView, Snapshot,
    SystemView, round1,
};
use crate::notify::{Notice, Notifier};
use crate::orphans::{LiveContainer, LiveSession, OrphanTracker};
use crate::procscan::{self, Proc, ProcKey, Scanner};
use crate::roots::Roots;
use crate::system;

const SPARK_SECS: f64 = 600.0;
const SPARK_POINTS: usize = 72;
/// Direct children started this soon after the session root are its
/// infrastructure (MCP servers, plugins), not tool activity.
const INFRA_WINDOW_SECS: u64 = 10;
const MAX_SESSION_PROCS: usize = 25;
const MAX_APPS: usize = 12;
const MAX_APP_PROCS: usize = 8;
const CMD_WIDTH: usize = 200;

#[derive(Default)]
struct Spark(VecDeque<Point>);

impl Spark {
    fn push(&mut self, p: Point) {
        self.0.push_back(p);
        while self.0.front().is_some_and(|f| p[0] - f[0] > SPARK_SECS) {
            self.0.pop_front();
        }
    }

    /// At most `max` points, averaging neighbours: a sparkline is ~72 px wide,
    /// so sending all 300 samples only bloats every detailed snapshot.
    fn points(&self, max: usize) -> Vec<Point> {
        let n = self.0.len();
        if n <= max {
            return self.0.iter().copied().collect();
        }
        let per = n.div_ceil(max);
        let all: Vec<Point> = self.0.iter().copied().collect();
        all.chunks(per)
            .map(|c| {
                let k = c.len() as f64;
                let last = c[c.len() - 1][0];
                [
                    last,
                    round1(c.iter().map(|p| p[1]).sum::<f64>() / k),
                    (c.iter().map(|p| p[2]).sum::<f64>() / k).round(),
                ]
            })
            .collect()
    }
}

pub struct TickResult {
    pub snapshot: Snapshot,
    pub targets: KillTargets,
    pub notices: Vec<Notice>,
}

pub struct Sampler {
    pub roots: Roots,
    pub config: Config,
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    history_path: PathBuf,
    home: String,
    scanner: Scanner,
    units: UnitScanner,
    docker: DockerScanner,
    metas: MetaCache,
    cmdlines: HashMap<ProcKey, Arc<str>>,
    tooling: HashMap<ProcKey, bool>,
    proc_units: HashMap<ProcKey, Option<Arc<UnitRef>>>,
    scratchpads: HashMap<String, (Option<String>, Instant)>,
    session_cwds: HashMap<String, String>,
    orphans: OrphanTracker,
    prev_cpu: Option<system::CpuTimes>,
    last_tick: Option<Instant>,
    sys_spark: Spark,
    session_sparks: HashMap<String, Spark>,
    history: Option<History>,
    notifier: Notifier,
}

impl Sampler {
    pub fn new(roots: Roots, config_path: PathBuf, history_path: PathBuf) -> Self {
        let (config, config_mtime) = Config::load(&config_path);
        let home = roots.home.to_string_lossy().into_owned();
        let mut s = Self {
            roots,
            config,
            config_path,
            config_mtime,
            history_path,
            home,
            scanner: Scanner::new(),
            units: UnitScanner::default(),
            docker: DockerScanner::default(),
            metas: MetaCache::default(),
            cmdlines: HashMap::new(),
            tooling: HashMap::new(),
            proc_units: HashMap::new(),
            scratchpads: HashMap::new(),
            session_cwds: HashMap::new(),
            orphans: OrphanTracker::default(),
            prev_cpu: None,
            last_tick: None,
            sys_spark: Spark::default(),
            session_sparks: HashMap::new(),
            history: None,
            notifier: Notifier::default(),
        };
        s.sync_history();
        s
    }

    pub fn interval(&self, active: bool) -> Duration {
        Duration::from_millis(if active {
            self.config.interval_active_ms
        } else {
            self.config.interval_idle_ms
        })
    }

    /// Writes any pending history; call before exiting.
    pub fn shutdown(&mut self) {
        if let Some(h) = &mut self.history
            && let Err(e) = h.flush()
        {
            eprintln!("hivetopd: history flush failed: {e}");
        }
    }

    fn sync_history(&mut self) {
        match (self.config.history.enabled, self.history.is_some()) {
            (true, false) => match History::open(&self.history_path, &self.config.history) {
                Ok(h) => self.history = Some(h),
                Err(e) => eprintln!(
                    "hivetopd: history disabled, cannot open {}: {e}",
                    self.history_path.display()
                ),
            },
            (false, true) => {
                self.shutdown();
                self.history = None;
            }
            _ => {}
        }
    }

    fn reload_config_if_changed(&mut self) {
        let mtime = fs::metadata(&self.config_path)
            .and_then(|m| m.modified())
            .ok();
        if mtime == self.config_mtime {
            return;
        }
        let (config, mtime) = Config::load(&self.config_path);
        let history_changed = config.history.rollup_secs != self.config.history.rollup_secs
            || config.history.retention_days != self.config.history.retention_days
            || config.history.downsample_after_days != self.config.history.downsample_after_days
            || config.history.top_apps != self.config.history.top_apps;
        self.config = config;
        self.config_mtime = mtime;
        if history_changed {
            self.shutdown();
            self.history = None;
        }
        self.sync_history();
        eprintln!("hivetopd: reloaded {}", self.config_path.display());
    }

    fn cmdline(&mut self, p: &Proc) -> Arc<str> {
        let proc_root = &self.roots.proc;
        self.cmdlines
            .entry(p.key())
            .or_insert_with(|| {
                procscan::read_cmdline(proc_root, p.pid)
                    .unwrap_or_else(|| format!("[{}]", p.comm))
                    .into()
            })
            .clone()
    }

    fn proc_view(&mut self, p: &Proc, infra: bool) -> ProcView {
        let cmd = self.cmdline(p);
        ProcView {
            pid: p.pid,
            start: p.start,
            comm: p.comm.clone(),
            cmd: format::short_command(&cmd, &self.home, CMD_WIDTH),
            rss: p.rss,
            cpu: round1(p.cpu),
            infra,
        }
    }

    fn is_tooling(&mut self, p: &Proc) -> bool {
        if let Some(&flag) = self.tooling.get(&p.key()) {
            return flag;
        }
        let cmd = procscan::read_cmdline(&self.roots.proc, p.pid).unwrap_or_default();
        let flag = cmd.contains("/.claude/");
        if flag {
            self.cmdlines.insert(p.key(), cmd.into());
        }
        self.tooling.insert(p.key(), flag);
        flag
    }

    /// `<claude tmp>/<project>/<session id>/scratchpad`, looked up again every
    /// 30 s until it exists (Claude Code creates it lazily).
    fn scratchpad(&mut self, key: &str, session_id: &str) -> Option<String> {
        if session_id.is_empty() || session_id.contains('/') || session_id.contains("..") {
            return None;
        }
        let now = Instant::now();
        if let Some((found, checked)) = self.scratchpads.get(key)
            && (found.is_some() || now.duration_since(*checked) < Duration::from_secs(30))
        {
            return found.clone();
        }
        let found = fs::read_dir(&self.roots.claude_tmp)
            .ok()
            .and_then(|dir| {
                dir.flatten()
                    .map(|e| e.path().join(session_id).join("scratchpad"))
                    .find(|p| p.is_dir())
            })
            .map(|p| p.to_string_lossy().into_owned());
        self.scratchpads
            .insert(key.to_string(), (found.clone(), now));
        found
    }

    pub fn tick(&mut self, detail: bool) -> TickResult {
        self.reload_config_if_changed();
        let now = Instant::now();
        let elapsed = self
            .last_tick
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(0.0);
        self.last_tick = Some(now);
        let ts = crate::unix_now();
        let clk = self.scanner.clk_tck;

        let stat = fs::read_to_string(self.roots.proc.join("stat")).unwrap_or_default();
        let (cpu_times, ncpu) =
            system::parse_cpu_times(&stat).unwrap_or((system::CpuTimes::default(), 1));
        let btime = system::parse_btime(&stat);
        let sys_cpu = self
            .prev_cpu
            .map(|p| system::cpu_percent(p, cpu_times))
            .unwrap_or(0.0);
        self.prev_cpu = Some(cpu_times);
        let mem = system::parse_meminfo(
            &fs::read_to_string(self.roots.proc.join("meminfo")).unwrap_or_default(),
        );
        let psi = system::read_psi(&self.roots.proc);

        let procs = self
            .scanner
            .scan(&self.roots.proc, elapsed, Some(self.roots.uid));
        let children = procscan::children_map(&procs);
        let alive = |k: &ProcKey| procs.get(&k.0).is_some_and(|p| p.start == k.1);
        self.cmdlines.retain(|k, _| alive(k));
        self.tooling.retain(|k, _| alive(k));
        self.proc_units.retain(|k, _| alive(k));

        // The user unit each process lives in (read once per process) and the app it belongs to.
        for p in procs.values() {
            if !self.proc_units.contains_key(&p.key()) {
                let unit = procscan::read_cgroup(&self.roots.proc, p.pid)
                    .and_then(|cg| cgroup::unit_from_cgroup_path(&cg))
                    .and_then(|(kind, name)| {
                        cgroup::parse_app_id(&name).map(|id| Arc::new(UnitRef::new(kind, name, id)))
                    });
                self.proc_units.insert(p.key(), unit);
            }
        }
        let mut units = self.units.scan(&self.roots.cgroup, self.roots.uid, elapsed);
        let unit_refs: HashMap<String, Arc<UnitRef>> = units
            .values()
            .map(|u| {
                (
                    u.name.clone(),
                    Arc::new(UnitRef::new(u.kind, u.name.clone(), u.id.clone())),
                )
            })
            .collect();
        let alias = apps::aliases(unit_refs.values().map(|u| u.as_ref()));
        for (from, to) in &alias {
            if let Some(a) = units.remove(from)
                && let Some(t) = units.get_mut(to)
            {
                t.mem.mem += a.mem.mem;
                t.mem.anon += a.mem.anon;
                t.mem.shmem += a.mem.shmem;
                t.cpu += a.cpu;
            }
        }
        let unit_of: HashMap<i32, Arc<UnitRef>> = procs
            .values()
            .filter_map(|p| {
                let raw = self.proc_units.get(&p.key())?.clone()?;
                let name = alias.get(&raw.name).unwrap_or(&raw.name);
                Some((p.pid, unit_refs.get(name).cloned().unwrap_or(raw)))
            })
            .collect();
        let user_manager = procs
            .values()
            .filter(|p| p.comm == "systemd")
            .map(|p| p.pid)
            .min();
        let owner = apps::assign(&procs, &unit_of, user_manager);

        // Claude sessions.
        let metas = self.metas.refresh(&self.roots.claude_sessions);
        let mut sessions: Vec<SessionView> = Vec::new();
        let mut members_by_key: HashMap<String, Vec<i32>> = HashMap::new();
        let mut in_session: HashSet<i32> = HashSet::new();
        for root in claude::find_roots(&procs, &metas) {
            let p = &procs[&root];
            let key = format!("{}:{}", p.pid, p.start);
            let meta = metas.get(&root).filter(|m| m.matches(p));
            let members = procscan::descendants(root, &children);
            let mut infra: HashSet<i32> = HashSet::new();
            let infra_limit = p.start + INFRA_WINDOW_SECS * clk;
            for &c in children.get(&root).into_iter().flatten() {
                if procs[&c].start <= infra_limit {
                    infra.insert(c);
                    infra.extend(procscan::descendants(c, &children));
                }
            }
            let cwd = match meta.map(|m| m.cwd.clone()).filter(|c| !c.is_empty()) {
                Some(c) => c,
                None => self
                    .session_cwds
                    .entry(key.clone())
                    .or_insert_with(|| {
                        procscan::read_cwd(&self.roots.proc, root)
                            .map(|c| c.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    })
                    .clone(),
            };
            let newest = children
                .get(&root)
                .into_iter()
                .flatten()
                .filter(|c| !infra.contains(c))
                .filter_map(|c| procs.get(c))
                .filter(|c| c.state != 'Z')
                .max_by_key(|c| c.start);
            let activity = newest.map(|c| {
                let cmd = self.cmdline(c);
                let text = claude::bash_tool_command(&cmd).unwrap_or_else(|| cmd.to_string());
                format::short_command(&text, &self.home, 80)
            });
            let entrypoint = meta
                .and_then(|m| m.entrypoint.clone())
                .unwrap_or_else(|| "unknown".into());
            let project = claude::project_name(&cwd);
            let app = owner.get(&root).map(|u| u.id.clone());
            let scratchpad =
                self.scratchpad(&key, meta.map(|m| m.session_id.as_str()).unwrap_or(""));
            let rss = p.rss
                + members
                    .iter()
                    .filter_map(|m| procs.get(m))
                    .map(|m| m.rss)
                    .sum::<u64>();
            let cpu = p.cpu
                + members
                    .iter()
                    .filter_map(|m| procs.get(m))
                    .map(|m| m.cpu)
                    .sum::<f64>();
            let mut processes = Vec::new();
            if detail {
                let mut list: Vec<&Proc> = std::iter::once(p)
                    .chain(members.iter().filter_map(|m| procs.get(m)))
                    .collect();
                list.sort_by_key(|p| std::cmp::Reverse(p.rss));
                for q in list.into_iter().take(MAX_SESSION_PROCS) {
                    let v = self.proc_view(q, infra.contains(&q.pid));
                    processes.push(v);
                }
            }
            in_session.insert(root);
            in_session.extend(members.iter().copied());
            let status = meta.and_then(|m| m.status.clone()).unwrap_or_else(|| {
                if activity.is_some() {
                    "busy".into()
                } else {
                    "idle".into()
                }
            });
            sessions.push(SessionView {
                id: meta
                    .map(|m| m.session_id.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| key.clone()),
                pid: root,
                name: meta
                    .and_then(|m| m.name.clone())
                    .unwrap_or_else(|| project.clone()),
                project,
                cwd,
                status,
                background: entrypoint == "sdk-cli",
                entrypoint,
                started_at: meta
                    .and_then(|m| m.started_at)
                    .map(|ms| ms / 1000)
                    .unwrap_or(btime + p.start / clk),
                app,
                procs: members.len() + 1,
                rss,
                cpu: round1(cpu),
                activity,
                scratchpad,
                ancestors: procscan::ancestors(root, &procs),
                processes,
                key: key.clone(),
                ..Default::default()
            });
            members_by_key.insert(key, members);
        }
        let live_keys: HashSet<&str> = sessions.iter().map(|s| s.key.as_str()).collect();
        self.session_cwds
            .retain(|k, _| live_keys.contains(k.as_str()));
        self.scratchpads
            .retain(|k, _| live_keys.contains(k.as_str()));
        self.session_sparks
            .retain(|k, _| live_keys.contains(k.as_str()));

        // Containers.
        let mut containers = if self.config.docker.enabled {
            let refresh = Duration::from_secs(self.config.docker.refresh_secs);
            self.docker.scan(
                &self.roots.cgroup,
                &self.config.docker.socket,
                refresh,
                elapsed,
            )
        } else {
            Vec::new()
        };
        for c in &mut containers {
            c.cpu = round1(c.cpu);
            c.session = docker::attribute(
                &c.working_dir,
                sessions
                    .iter()
                    .filter(|s| !s.background)
                    .map(|s| (s.key.as_str(), s.cwd.as_str(), s.started_at)),
            );
            if let Some(s) = sessions
                .iter_mut()
                .find(|s| Some(&s.key) == c.session.as_ref())
            {
                s.containers.push(c.id.clone());
                s.container_mem += c.mem;
                s.container_cpu = round1(s.container_cpu + c.cpu);
            }
        }

        // Orphans.
        let live: Vec<LiveSession> = sessions
            .iter()
            .map(|s| LiveSession {
                key: &s.key,
                id: &s.id,
                name: &s.name,
                cwd: &s.cwd,
                members: members_by_key.get(&s.key).map(Vec::as_slice).unwrap_or(&[]),
            })
            .collect();
        let live_containers: Vec<LiveContainer> = containers
            .iter()
            .map(|c| LiveContainer {
                id: &c.id,
                oneoff: c.oneoff,
                session_key: c.session.as_deref(),
            })
            .collect();
        let groups = self
            .orphans
            .update(ts, &live, &live_containers, &procs, &children);
        drop(live);
        drop(live_containers);
        let mut orphans = Vec::new();
        let mut orphan_pids: HashSet<i32> = HashSet::new();
        let mut targets = KillTargets::default();
        for g in groups {
            let Some(origin) = g.origin else { continue };
            let list: Vec<&Proc> = g.pids.iter().filter_map(|pid| procs.get(pid)).collect();
            orphan_pids.extend(list.iter().map(|p| p.pid));
            let mut processes = Vec::new();
            if detail {
                let mut sorted = list.clone();
                sorted.sort_by_key(|p| std::cmp::Reverse(p.rss));
                for q in sorted.into_iter().take(MAX_SESSION_PROCS) {
                    let v = self.proc_view(q, false);
                    processes.push(v);
                }
            }
            for c in containers
                .iter_mut()
                .filter(|c| g.containers.contains(&c.id))
            {
                c.orphan_of = Some(origin.session_id.clone());
            }
            let mut keys: Vec<ProcKey> = list.iter().map(|p| p.key()).collect();
            keys.sort_by_key(|k| std::cmp::Reverse(k.1));
            targets
                .orphans
                .insert(origin.session_id.clone(), (keys, g.containers.clone()));
            orphans.push(OrphanView {
                session_id: origin.session_id,
                name: origin.name,
                cwd: origin.cwd,
                ended_at: origin.ended_at,
                procs: list.len(),
                rss: list.iter().map(|p| p.rss).sum(),
                cpu: round1(list.iter().map(|p| p.cpu).sum()),
                containers: g.containers,
                processes,
            });
        }

        // Shared Claude tooling outside every session.
        let mut tooling = GroupView::default();
        let mut tooling_procs: Vec<&Proc> = Vec::new();
        for p in procs.values() {
            if in_session.contains(&p.pid) || orphan_pids.contains(&p.pid) {
                continue;
            }
            if self.is_tooling(p) {
                tooling_procs.push(p);
            }
        }
        tooling.procs = tooling_procs.len();
        tooling.rss = tooling_procs.iter().map(|p| p.rss).sum();
        tooling.cpu = round1(tooling_procs.iter().map(|p| p.cpu).sum());
        if detail {
            tooling_procs.sort_by_key(|p| std::cmp::Reverse(p.rss));
            for q in tooling_procs.into_iter().take(MAX_SESSION_PROCS) {
                let v = self.proc_view(q, false);
                tooling.processes.push(v);
            }
        }

        // Apps: cgroup usage per unit, then moved along with the processes that
        // sit in another unit's cgroup (see `apps`). Their share is estimated
        // from RssAnon + RssShmem, capped by what the host cgroup holds.
        let mut strays: HashMap<&str, Vec<(&Proc, &str)>> = HashMap::new();
        let mut owned: HashMap<&str, usize> = HashMap::new();
        for (pid, target) in &owner {
            *owned.entry(target.name.as_str()).or_default() += 1;
            if let (Some(own), Some(p)) = (unit_of.get(pid), procs.get(pid))
                && own.name != target.name
            {
                strays
                    .entry(own.name.as_str())
                    .or_default()
                    .push((p, target.name.as_str()));
            }
        }
        let mut adopted: HashMap<&str, usize> = HashMap::new();
        for (host, list) in &strays {
            let estimates: Vec<u64> = list
                .iter()
                .map(|(p, _)| procscan::read_rss_owned(&self.roots.proc, p.pid))
                .collect();
            let total: u64 = estimates.iter().sum();
            let movable = units
                .get(*host)
                .map(|u| u.mem.anon + u.mem.shmem)
                .unwrap_or(0);
            let scale = if total > movable && total > 0 {
                movable as f64 / total as f64
            } else {
                1.0
            };
            let (mut moved_mem, mut moved_cpu) = (0u64, 0.0);
            for ((p, target), estimate) in list.iter().zip(estimates) {
                let m = (estimate as f64 * scale) as u64;
                moved_mem += m;
                moved_cpu += p.cpu;
                if let Some(t) = units.get_mut(*target) {
                    t.mem.mem += m;
                    t.cpu += p.cpu;
                }
                *adopted.entry(target).or_default() += 1;
            }
            let emptied = owned.get(host).copied().unwrap_or(0) == 0;
            let first_target = list.first().map(|(_, t)| *t);
            if let Some(h) = units.get_mut(*host) {
                h.mem.mem = h.mem.mem.saturating_sub(moved_mem);
                h.cpu = (h.cpu - moved_cpu).max(0.0);
                if emptied {
                    let (rest_mem, rest_cpu) = (h.mem.mem, h.cpu);
                    h.mem.mem = 0;
                    h.cpu = 0.0;
                    if let Some(t) = first_target.and_then(|t| units.get_mut(t)) {
                        t.mem.mem += rest_mem;
                        t.cpu += rest_cpu;
                    }
                }
            }
        }
        let mut pids_by_id: HashMap<&str, Vec<&Proc>> = HashMap::new();
        for (pid, u) in &owner {
            if let Some(p) = procs.get(pid) {
                pids_by_id.entry(u.id.as_str()).or_default().push(p);
            }
        }
        let mut by_id: HashMap<String, AppView> = HashMap::new();
        for u in units.values() {
            let e = by_id.entry(u.id.clone()).or_insert_with(|| AppView {
                id: u.id.clone(),
                kind: u.kind.to_string(),
                ..Default::default()
            });
            e.mem += u.mem.mem;
            e.cpu += u.cpu;
            e.adopted += adopted.get(u.name.as_str()).copied().unwrap_or(0);
            if u.kind == "app" {
                e.kind = "app".into();
            }
        }
        let mut all_apps: Vec<AppView> = by_id.into_values().collect();
        all_apps.sort_by_key(|a| std::cmp::Reverse(a.mem));
        let mut apps = Vec::new();
        for mut a in all_apps
            .iter()
            .filter(|a| a.mem > 0)
            .take(MAX_APPS)
            .cloned()
        {
            let mut list = pids_by_id.remove(a.id.as_str()).unwrap_or_default();
            a.procs = list.len();
            a.cpu = round1(a.cpu);
            a.sessions = sessions
                .iter()
                .filter(|s| s.app.as_deref() == Some(a.id.as_str()))
                .count();
            if detail {
                list.sort_by_key(|p| std::cmp::Reverse(p.rss));
                for q in list.into_iter().take(MAX_APP_PROCS) {
                    let v = self.proc_view(q, false);
                    a.processes.push(v);
                }
            }
            apps.push(a);
        }

        // Sparklines.
        let mem_pct = if mem.total > 0 {
            100.0 * mem.used() as f64 / mem.total as f64
        } else {
            0.0
        };
        self.sys_spark
            .push([ts as f64, round1(sys_cpu), round1(mem_pct)]);
        for s in &mut sessions {
            let spark = self.session_sparks.entry(s.key.clone()).or_default();
            spark.push([ts as f64, s.cpu, s.rss as f64]);
            if detail {
                s.spark = spark.points(SPARK_POINTS);
            }
        }

        // History.
        if let Some(h) = &mut self.history {
            if let Err(e) = h.tick(ts) {
                eprintln!("hivetopd: history write failed: {e}");
            }
            h.record("system", "system", "System", sys_cpu, mem.used() as f64);
            for s in &sessions {
                h.record("session", &s.id, &s.name, s.cpu, s.rss as f64);
            }
            for a in &all_apps {
                h.record("app", &a.id, &a.id, a.cpu, a.mem as f64);
            }
            for c in &containers {
                h.record("container", &c.name, &c.name, c.cpu, c.mem as f64);
            }
        }

        // Kill targets.
        targets.procs = procs
            .values()
            .map(|p| (p.pid, (p.start, p.comm.clone())))
            .collect();
        for s in &sessions {
            let members = members_by_key.get(&s.key).map(Vec::as_slice).unwrap_or(&[]);
            let mut keys: Vec<ProcKey> = members
                .iter()
                .rev()
                .filter_map(|m| procs.get(m))
                .map(Proc::key)
                .collect();
            keys.push(procs[&s.pid].key());
            let oneoffs = containers
                .iter()
                .filter(|c| c.oneoff && c.session.as_ref() == Some(&s.key))
                .map(|c| c.id.clone())
                .collect();
            targets.sessions.insert(s.key.clone(), (keys, oneoffs));
        }
        targets.containers = containers
            .iter()
            .filter(|c| c.session.is_some() || c.orphan_of.is_some())
            .map(|c| c.id.clone())
            .collect();

        sessions.sort_by(|a, b| a.background.cmp(&b.background).then(b.rss.cmp(&a.rss)));
        let snapshot = Snapshot {
            ts,
            ncpu,
            detail,
            system: SystemView {
                cpu: round1(sys_cpu),
                mem_total: mem.total,
                mem_used: mem.used(),
                swap_total: mem.swap_total,
                swap_used: mem.swap_used(),
                psi,
                pressure: system::pressure_level(&psi),
                spark: if detail {
                    self.sys_spark.points(SPARK_POINTS)
                } else {
                    Vec::new()
                },
            },
            sessions,
            tooling,
            orphans,
            apps,
            containers,
            docker_ok: self.docker.ok,
        };
        let notices = self.notifier.evaluate(&self.config.notify, &snapshot, now);
        TickResult {
            snapshot,
            targets,
            notices,
        }
    }
}

#[cfg(test)]
mod spark_tests {
    use super::*;

    #[test]
    fn downsampled_points() {
        let mut s = Spark::default();
        for i in 0..300 {
            s.push([i as f64, 10.0, 100.0]);
        }
        let p = s.points(72);
        assert!(p.len() <= 72 && p.len() >= 60);
        assert_eq!(p.last().unwrap()[0], 299.0);
        assert_eq!(p[0][1], 10.0);
    }
}
