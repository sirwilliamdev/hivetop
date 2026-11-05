use std::error::Error;
use std::io::{self, BufRead, Write};

use serde_json::Value;

use hivetop::format::{bytes, duration, truncate};
use hivetop::procscan;
use hivetop::{BUS_NAME, INTERFACE, OBJECT_PATH};

const USAGE: &str = "usage:
  hivetop status [--json]
  hivetop history [--range 1h|24h|7d|30d] [--target kind:key] [--json]
  hivetop kill <session name|session key|pid> [--yes]";

type Res<T> = Result<T, Box<dyn Error>>;

fn call<B>(conn: &zbus::blocking::Connection, method: &str, body: &B) -> Res<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    Ok(conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, body)?)
}

fn snapshot(conn: &zbus::blocking::Connection) -> Res<Value> {
    call(conn, "SetActive", &(true,))?;
    let text: String = call(conn, "GetSnapshot", &(true,))?.body().deserialize()?;
    call(conn, "SetActive", &(false,))?;
    Ok(serde_json::from_str(&text)?)
}

fn num(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(0.0)
}

fn u(v: &Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn list<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v[k].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn print_status(snap: &Value) {
    let sys = &snap["system"];
    let total = u(sys, "mem_total").max(1);
    let pressure = ["calm", "WARNING", "CRITICAL"][(u(sys, "pressure") as usize).min(2)];
    println!(
        "System  CPU {:.0}% of {} cores   RAM {} / {} ({:.0}%)   Swap {}   Pressure {}",
        num(sys, "cpu"),
        u(snap, "ncpu"),
        bytes(u(sys, "mem_used")),
        bytes(total),
        100.0 * u(sys, "mem_used") as f64 / total as f64,
        bytes(u(sys, "swap_used")),
        pressure
    );
    let now = u(snap, "ts");
    for (title, background) in [("Claude sessions", false), ("Background sessions", true)] {
        let rows: Vec<&Value> = list(snap, "sessions")
            .iter()
            .filter(|x| x["background"].as_bool() == Some(background))
            .collect();
        if rows.is_empty() {
            continue;
        }
        println!("\n{title} ({})", rows.len());
        println!(
            "  {:<28} {:>9} {:>7} {:>5} {:>6} {:>7}  NOW",
            "NAME", "RAM", "CPU", "PROCS", "STATUS", "AGE"
        );
        for x in rows {
            println!(
                "  {:<28} {:>9} {:>6.0}% {:>5} {:>6} {:>7}  {}",
                truncate(s(x, "name"), 28),
                bytes(u(x, "rss")),
                num(x, "cpu"),
                u(x, "procs"),
                s(x, "status"),
                duration(now.saturating_sub(u(x, "started_at"))),
                x["activity"].as_str().unwrap_or("-")
            );
            let containers = list(x, "containers");
            if !containers.is_empty() {
                println!(
                    "  {:<28} {:>9} {:>6.0}%  {} container(s)",
                    "  + docker",
                    bytes(u(x, "container_mem")),
                    num(x, "container_cpu"),
                    containers.len()
                );
            }
        }
    }
    let tooling = &snap["tooling"];
    if u(tooling, "procs") > 0 {
        println!(
            "\nShared Claude tooling: {} process(es), {}, {:.0}% CPU",
            u(tooling, "procs"),
            bytes(u(tooling, "rss")),
            num(tooling, "cpu")
        );
    }
    let orphans = list(snap, "orphans");
    if !orphans.is_empty() {
        println!("\nOrphans (left behind by ended sessions)");
        for o in orphans {
            println!(
                "  {:<28} {:>9} {:>6.0}%  {} process(es), {} container(s), ended {} ago",
                truncate(s(o, "name"), 28),
                bytes(u(o, "rss")),
                num(o, "cpu"),
                u(o, "procs"),
                list(o, "containers").len(),
                duration(now.saturating_sub(u(o, "ended_at")))
            );
            for p in list(o, "processes").iter().take(5) {
                println!("      {:>7}  {}", p["pid"], s(p, "cmd"));
            }
        }
    }
    println!("\nApps");
    for a in list(snap, "apps") {
        let note = match u(a, "sessions") {
            0 => String::new(),
            n => format!("  incl. {n} Claude session(s)"),
        };
        println!(
            "  {:<36} {:>9} {:>6.0}%{note}",
            truncate(s(a, "id"), 36),
            bytes(u(a, "mem")),
            num(a, "cpu")
        );
    }
    let containers = list(snap, "containers");
    if !containers.is_empty() {
        println!("\nContainers");
        for c in containers {
            let owner = if let Some(o) = c["orphan_of"].as_str() {
                format!("orphan of {o}")
            } else if let Some(k) = c["session"].as_str() {
                let name = list(snap, "sessions")
                    .iter()
                    .find(|x| s(x, "key") == k)
                    .map(|x| s(x, "name"))
                    .unwrap_or(k);
                format!("session {name}")
            } else {
                String::new()
            };
            println!(
                "  {:<36} {:>9} {:>6.0}%  {owner}",
                truncate(s(c, "name"), 36),
                bytes(u(c, "mem")),
                num(c, "cpu")
            );
        }
    } else if snap["docker_ok"].as_bool() == Some(false) {
        println!("\nContainers: Docker not reachable");
    }
}

fn print_history(h: &Value) {
    if let Some(err) = h["error"].as_str() {
        println!("history unavailable: {err}");
        return;
    }
    let points = list(h, "points");
    println!("{} history, {} points", s(h, "range"), points.len());
    if !points.is_empty() {
        let cpu: Vec<f64> = points
            .iter()
            .map(|p| p[1].as_f64().unwrap_or(0.0))
            .collect();
        let max_cpu = cpu.iter().cloned().fold(0.0, f64::max);
        let peak_mem = points
            .iter()
            .map(|p| p[4].as_f64().unwrap_or(0.0))
            .fold(0.0, f64::max);
        println!("CPU  {}  peak {:.0}%", spark(&cpu), max_cpu);
        let mem: Vec<f64> = points
            .iter()
            .map(|p| p[3].as_f64().unwrap_or(0.0))
            .collect();
        println!("RAM  {}  peak {}", spark(&mem), bytes(peak_mem as u64));
    }
    println!(
        "\n  {:<10} {:<34} {:>9} {:>9} {:>7} {:>7}",
        "KIND", "NAME", "RAM AVG", "RAM PEAK", "CPU AVG", "MINUTES"
    );
    for r in list(h, "ranking") {
        println!(
            "  {:<10} {:<34} {:>9} {:>9} {:>6.0}% {:>7}",
            s(r, "kind"),
            truncate(s(r, "label"), 34),
            bytes(num(r, "mem_avg") as u64),
            bytes(num(r, "mem_max") as u64),
            num(r, "cpu_avg"),
            u(r, "minutes")
        );
    }
}

fn spark(values: &[f64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().cloned().fold(0.0, f64::max);
    let step = values.len().div_ceil(60).max(1);
    values
        .chunks(step)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .map(|v| {
            if max > 0.0 {
                BARS[((v / max) * 7.0).round() as usize]
            } else {
                BARS[0]
            }
        })
        .collect()
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
}

fn kill(conn: &zbus::blocking::Connection, target: &str, yes: bool) -> Res<()> {
    let snap = snapshot(conn)?;
    let (kind, id, label) = if let Ok(pid) = target.parse::<i32>() {
        let start = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|t| procscan::parse_stat(&t))
            .map(|st| st.start)
            .ok_or("no such process")?;
        ("pid", format!("{pid}:{start}"), format!("process {pid}"))
    } else {
        let matches: Vec<&Value> = list(&snap, "sessions")
            .iter()
            .filter(|x| [s(x, "name"), s(x, "key"), s(x, "id")].contains(&target))
            .collect();
        match matches.as_slice() {
            [one] => (
                "session",
                s(one, "key").to_string(),
                format!("session {} ({} processes)", s(one, "name"), u(one, "procs")),
            ),
            [] => return Err(format!("no session named {target}").into()),
            many => {
                let keys: Vec<&str> = many.iter().map(|x| s(x, "key")).collect();
                return Err(
                    format!("{target} is ambiguous, use a key: {}", keys.join(", ")).into(),
                );
            }
        }
    };
    if !yes && !confirm(&format!("Terminate {label}?")) {
        return Ok(());
    }
    let done: bool = call(conn, "Kill", &(kind, id.as_str()))?
        .body()
        .deserialize()?;
    println!(
        "{}",
        if done {
            "SIGTERM sent (SIGKILL in 5 s if still alive)"
        } else {
            "target is gone"
        }
    );
    Ok(())
}

fn main() -> Res<()> {
    // Exit quietly when stdout closes early (`hivetop status | head`).
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let Some(cmd) = args.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    let conn = zbus::blocking::Connection::session()?;
    match cmd.as_str() {
        "status" => {
            let snap = snapshot(&conn)?;
            if flag("--json") {
                println!("{}", serde_json::to_string_pretty(&snap)?);
            } else {
                print_status(&snap);
            }
        }
        "history" => {
            let range = opt("--range").unwrap_or_else(|| "24h".into());
            let target = opt("--target").unwrap_or_default();
            let text: String = call(&conn, "GetHistory", &(range.as_str(), target.as_str()))?
                .body()
                .deserialize()?;
            let h: Value = serde_json::from_str(&text)?;
            if flag("--json") {
                println!("{}", serde_json::to_string_pretty(&h)?);
            } else {
                print_history(&h);
            }
        }
        "kill" => {
            let target = args.get(1).ok_or(USAGE)?;
            kill(&conn, target, flag("--yes"))?;
        }
        "-V" | "--version" => println!("hivetop {}", env!("CARGO_PKG_VERSION")),
        _ => println!("{USAGE}"),
    }
    Ok(())
}
