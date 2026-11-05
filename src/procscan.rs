use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// A process identity that survives PID reuse: (pid, start time in clock ticks).
pub type ProcKey = (i32, u64);

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub comm: String,
    pub state: char,
    pub start: u64,
    pub ticks: u64,
    pub rss: u64,
    /// Percent of one core over the last interval.
    pub cpu: f64,
}

impl Proc {
    pub fn key(&self) -> ProcKey {
        (self.pid, self.start)
    }
}

#[derive(Debug, PartialEq)]
pub struct StatFields {
    pub ppid: i32,
    pub comm: String,
    pub state: char,
    pub ticks: u64,
    pub start: u64,
    pub rss_pages: u64,
}

/// Parses `/proc/<pid>/stat`. `comm` sits in parentheses and may contain
/// spaces or parentheses itself, so fields are counted from the last `)`.
pub fn parse_stat(s: &str) -> Option<StatFields> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = s.get(close + 1..)?.split_whitespace().collect();
    let field = |n: usize| rest.get(n - 3).and_then(|v| v.parse::<u64>().ok());
    Some(StatFields {
        state: rest.first()?.chars().next()?,
        ppid: rest.get(1)?.parse().ok()?,
        comm,
        ticks: field(14)? + field(15)?,
        start: field(22)?,
        rss_pages: field(24)?,
    })
}

pub struct Scanner {
    pub clk_tck: u64,
    page_size: u64,
    prev: HashMap<ProcKey, u64>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        let clk = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Self {
            clk_tck: if clk > 0 { clk as u64 } else { 100 },
            page_size: if page > 0 { page as u64 } else { 4096 },
            prev: HashMap::new(),
        }
    }

    /// Reads every process once, only those owned by `uid` when given (sessions,
    /// apps and kill targets are all per-user). Kernel threads are skipped.
    pub fn scan(
        &mut self,
        proc_root: &Path,
        elapsed_secs: f64,
        uid: Option<u32>,
    ) -> HashMap<i32, Proc> {
        let mut procs = HashMap::with_capacity(self.prev.len() + 32);
        let mut next = HashMap::with_capacity(self.prev.len() + 32);
        let Ok(dir) = fs::read_dir(proc_root) else {
            return procs;
        };
        let mut buf = String::with_capacity(512);
        let mut path = PathBuf::from(proc_root);
        for entry in dir.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|n| n.parse::<i32>().ok()) else {
                continue;
            };
            if let Some(uid) = uid
                && entry.metadata().map(|m| m.uid()).ok() != Some(uid)
            {
                continue;
            }
            buf.clear();
            path.push(&name);
            path.push("stat");
            let ok = fs::File::open(&path)
                .and_then(|mut f| f.read_to_string(&mut buf))
                .is_ok();
            path.pop();
            path.pop();
            if !ok {
                continue;
            }
            let Some(st) = parse_stat(&buf) else {
                continue;
            };
            if pid == 2 || st.ppid == 2 {
                continue;
            }
            let key = (pid, st.start);
            let cpu = match self.prev.get(&key) {
                Some(&prev) if elapsed_secs > 0.0 => {
                    st.ticks.saturating_sub(prev) as f64 / self.clk_tck as f64 / elapsed_secs
                        * 100.0
                }
                _ => 0.0,
            };
            next.insert(key, st.ticks);
            procs.insert(
                pid,
                Proc {
                    pid,
                    ppid: st.ppid,
                    comm: st.comm,
                    state: st.state,
                    start: st.start,
                    ticks: st.ticks,
                    rss: st.rss_pages * self.page_size,
                    cpu,
                },
            );
        }
        self.prev = next;
        procs
    }
}

pub fn children_map(procs: &HashMap<i32, Proc>) -> HashMap<i32, Vec<i32>> {
    let mut map: HashMap<i32, Vec<i32>> = HashMap::with_capacity(procs.len());
    for p in procs.values() {
        map.entry(p.ppid).or_default().push(p.pid);
    }
    for kids in map.values_mut() {
        kids.sort_unstable();
    }
    map
}

/// All descendants of `root` in pre-order (parents before their children), root excluded.
pub fn descendants(root: i32, children: &HashMap<i32, Vec<i32>>) -> Vec<i32> {
    let mut out = Vec::new();
    let mut stack: Vec<i32> = children
        .get(&root)
        .map(|k| k.iter().rev().copied().collect())
        .unwrap_or_default();
    while let Some(pid) = stack.pop() {
        if out.len() > 100_000 {
            break;
        }
        out.push(pid);
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().rev());
        }
    }
    out
}

/// Parent chain of `pid`, nearest first, stopping before PID 1.
pub fn ancestors(pid: i32, procs: &HashMap<i32, Proc>) -> Vec<i32> {
    let mut out = Vec::new();
    let mut cur = procs.get(&pid).map(|p| p.ppid).unwrap_or(0);
    while cur > 1 && out.len() < 64 {
        out.push(cur);
        cur = procs.get(&cur).map(|p| p.ppid).unwrap_or(0);
    }
    out
}

pub fn read_cmdline(proc_root: &Path, pid: i32) -> Option<String> {
    let mut f = fs::File::open(proc_root.join(pid.to_string()).join("cmdline")).ok()?;
    let mut raw = Vec::with_capacity(256);
    f.by_ref().take(8192).read_to_end(&mut raw).ok()?;
    if raw.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&raw);
    let joined = text.trim_end_matches('\0').replace('\0', " ");
    Some(joined)
}

pub fn read_cwd(proc_root: &Path, pid: i32) -> Option<PathBuf> {
    fs::read_link(proc_root.join(pid.to_string()).join("cwd")).ok()
}

pub fn read_cgroup(proc_root: &Path, pid: i32) -> Option<String> {
    let s = fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup")).ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("0::").map(str::to_string))
}

/// Private plus shared-memory resident bytes (`RssAnon + RssShmem`), the part of
/// a process a cgroup charges for and that page-cache reclaim cannot drop.
pub fn read_rss_owned(proc_root: &Path, pid: i32) -> u64 {
    let Ok(s) = fs::read_to_string(proc_root.join(pid.to_string()).join("status")) else {
        return 0;
    };
    parse_rss_owned(&s)
}

pub fn parse_rss_owned(status: &str) -> u64 {
    status
        .lines()
        .filter(|l| l.starts_with("RssAnon:") || l.starts_with("RssShmem:"))
        .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        .sum::<u64>()
        * 1024
}

pub fn proc_uid(proc_root: &Path, pid: i32) -> Option<u32> {
    fs::metadata(proc_root.join(pid.to_string()))
        .ok()
        .map(|m| m.uid())
}

/// True when `pid` still exists with the same start time (guards against PID reuse).
pub fn still_alive(proc_root: &Path, key: ProcKey) -> bool {
    fs::read_to_string(proc_root.join(key.0.to_string()).join("stat"))
        .ok()
        .and_then(|s| parse_stat(&s))
        .is_some_and(|st| st.start == key.1 && st.state != 'Z')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, ppid: i32, comm: &str) -> Proc {
        Proc {
            pid,
            ppid,
            comm: comm.into(),
            state: 'S',
            start: pid as u64,
            ticks: 0,
            rss: 0,
            cpu: 0.0,
        }
    }

    #[test]
    fn stat_with_tricky_comm() {
        let s = "1234 (my (odd) proc) S 1 1234 1234 0 -1 4194560 100 0 0 0 7 3 0 0 20 0 1 0 685160 1000000 250 18446744073709551615";
        let st = parse_stat(s).unwrap();
        assert_eq!(st.comm, "my (odd) proc");
        assert_eq!(st.ppid, 1);
        assert_eq!(st.state, 'S');
        assert_eq!(st.ticks, 10);
        assert_eq!(st.start, 685160);
        assert_eq!(st.rss_pages, 250);
    }

    #[test]
    fn rss_owned_from_status() {
        let s = "Name:\tbrave\nVmRSS:\t 900 kB\nRssAnon:\t 300 kB\nRssFile:\t 500 kB\nRssShmem:\t 100 kB\n";
        assert_eq!(parse_rss_owned(s), 400 * 1024);
    }

    #[test]
    fn tree_helpers() {
        let procs: HashMap<i32, Proc> = [
            proc(10, 1, "claude"),
            proc(11, 10, "node"),
            proc(12, 11, "node"),
            proc(13, 10, "bash"),
            proc(20, 1, "other"),
        ]
        .into_iter()
        .map(|p| (p.pid, p))
        .collect();
        let children = children_map(&procs);
        assert_eq!(descendants(10, &children), vec![11, 12, 13]);
        assert!(descendants(20, &children).is_empty());
        assert_eq!(ancestors(12, &procs), vec![11, 10]);
    }
}
