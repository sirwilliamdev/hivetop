use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
}

/// Parses the aggregate `cpu` line of `/proc/stat` and counts the per-CPU lines.
pub fn parse_cpu_times(stat: &str) -> Option<(CpuTimes, usize)> {
    let mut times = None;
    let mut ncpu = 0;
    for line in stat.lines() {
        if let Some(rest) = line.strip_prefix("cpu ") {
            let v: Vec<u64> = rest
                .split_whitespace()
                .take(8)
                .filter_map(|x| x.parse().ok())
                .collect();
            if v.len() < 4 {
                return None;
            }
            let idle = v[3] + v.get(4).copied().unwrap_or(0);
            times = Some(CpuTimes {
                total: v.iter().sum(),
                idle,
            });
        } else if line.starts_with("cpu") && line.as_bytes().get(3).is_some_and(u8::is_ascii_digit)
        {
            ncpu += 1;
        }
    }
    times.map(|t| (t, ncpu.max(1)))
}

/// Boot time in unix seconds, from the `btime` line of `/proc/stat`.
pub fn parse_btime(stat: &str) -> u64 {
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

pub fn cpu_percent(prev: CpuTimes, cur: CpuTimes) -> f64 {
    let total = cur.total.saturating_sub(prev.total);
    if total == 0 {
        return 0.0;
    }
    let idle = cur.idle.saturating_sub(prev.idle).min(total);
    100.0 * (total - idle) as f64 / total as f64
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl Memory {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
}

pub fn parse_meminfo(s: &str) -> Memory {
    let mut m = Memory::default();
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(val)) = (it.next(), it.next()) else {
            continue;
        };
        let bytes = val.parse::<u64>().unwrap_or(0) * 1024;
        match key {
            "MemTotal:" => m.total = bytes,
            "MemAvailable:" => m.available = bytes,
            "SwapTotal:" => m.swap_total = bytes,
            "SwapFree:" => m.swap_free = bytes,
            _ => {}
        }
    }
    m
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Psi {
    pub cpu_some: f64,
    pub memory_some: f64,
    pub memory_full: f64,
    pub io_some: f64,
    pub io_full: f64,
}

pub fn parse_psi_avg10(s: &str, kind: &str) -> f64 {
    s.lines()
        .find(|l| l.starts_with(kind))
        .and_then(|l| l.split_whitespace().find_map(|f| f.strip_prefix("avg10=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

pub fn read_psi(proc_root: &Path) -> Psi {
    let read =
        |name: &str| fs::read_to_string(proc_root.join("pressure").join(name)).unwrap_or_default();
    let cpu = read("cpu");
    let mem = read("memory");
    let io = read("io");
    Psi {
        cpu_some: parse_psi_avg10(&cpu, "some"),
        memory_some: parse_psi_avg10(&mem, "some"),
        memory_full: parse_psi_avg10(&mem, "full"),
        io_some: parse_psi_avg10(&io, "some"),
        io_full: parse_psi_avg10(&io, "full"),
    }
}

/// 0 = calm, 1 = warning, 2 = critical.
pub fn pressure_level(p: &Psi) -> u8 {
    let level = |v: f64, warn: f64, crit: f64| {
        if v >= crit {
            2
        } else if v >= warn {
            1
        } else {
            0
        }
    };
    [
        level(p.memory_some, 10.0, 30.0),
        level(p.memory_full, 2.0, 10.0),
        level(p.cpu_some, 40.0, 80.0),
        level(p.io_full, 10.0, 30.0),
    ]
    .into_iter()
    .max()
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 50 0 25 400 25 0 0 0 0 0\ncpu1 50 0 25 400 25 0 0 0 0 0\nintr 1 2 3\n";

    #[test]
    fn cpu_times_and_count() {
        let (t, n) = parse_cpu_times(STAT).unwrap();
        assert_eq!(n, 2);
        assert_eq!(t.total, 1000);
        assert_eq!(t.idle, 850);
    }

    #[test]
    fn cpu_percent_from_delta() {
        let a = CpuTimes {
            total: 1000,
            idle: 800,
        };
        let b = CpuTimes {
            total: 1100,
            idle: 875,
        };
        assert!((cpu_percent(a, b) - 25.0).abs() < 1e-9);
        assert_eq!(cpu_percent(b, b), 0.0);
    }

    #[test]
    fn meminfo_in_bytes() {
        let m = parse_meminfo(
            "MemTotal:       1000 kB\nMemFree: 1 kB\nMemAvailable:    400 kB\nSwapTotal: 100 kB\nSwapFree: 40 kB\n",
        );
        assert_eq!(m.total, 1_024_000);
        assert_eq!(m.used(), 600 * 1024);
        assert_eq!(m.swap_used(), 60 * 1024);
    }

    #[test]
    fn psi_lines() {
        let s = "some avg10=12.50 avg60=1.00 avg300=0.00 total=1\nfull avg10=3.00 avg60=0.00 avg300=0.00 total=1\n";
        assert_eq!(parse_psi_avg10(s, "some"), 12.5);
        assert_eq!(parse_psi_avg10(s, "full"), 3.0);
        assert_eq!(parse_psi_avg10("", "some"), 0.0);
        let p = Psi {
            memory_some: 12.5,
            memory_full: 3.0,
            ..Default::default()
        };
        assert_eq!(pressure_level(&p), 1);
        assert_eq!(pressure_level(&Psi::default()), 0);
    }
}
