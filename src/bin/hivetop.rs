use std::error::Error;
use std::io::{self, BufRead, Write};

use serde_json::Value;

use hivetop::format::{bytes, duration, truncate};
use hivetop::procscan;
use hivetop::{BUS_NAME, INTERFACE, OBJECT_PATH};

const USAGE: &str = "usage:
  hivetop status [--json]
  hivetop history [--range 1h|24h|7d|30d] [--target kind:key] [--json]
  hivetop kill <agent name|title|key|pid> [--yes]";

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
    for (title, sdk) in [("AI agents", false), ("Background", true)] {
        let rows: Vec<&Value> = list(snap, "agents")
            .iter()
            .filter(|x| (s(x, "kind") == "sdk") == sdk)
            .collect();
        if rows.is_empty() {
            continue;
        }
        println!("\n{title} ({})", rows.len());
        println!(
            "  {:<34} {:<13} {:>6} {:>7} {:>5} {:>9} {:>6}  NOW",
            "TITLE", "STATE", "CTX", "TOKENS", "CPU", "RAM", "AGE"
        );
        for x in rows {
            let state = format!(
                "{} {}",
                s(x, "state"),
                duration(now.saturating_sub(u(x, "state_since")))
            );
            println!(
                "  {:<34} {:<13} {:>6} {:>7} {:>4.0}% {:>9} {:>6}  {}",
                truncate(s(x, "title"), 34),
                state,
                tokens(u(x, "context")),
                tokens(u(&x["tokens"], "total")),
                num(x, "cpu"),
                bytes(u(x, "rss")),
                duration(now.saturating_sub(u(x, "started_at"))),
                x["activity"].as_str().unwrap_or("-")
            );
            let mut about = vec![s(x, "project").to_string()];
            for k in ["model", "mode", "branch"] {
                if let Some(v) = x[k].as_str() {
                    about.push(v.to_string());
                }
            }
            if let Some(j) = x["job"].as_object() {
                about.push(format!("job {}", j["id"].as_str().unwrap_or("")));
            }
            let running = list(x, "subagents")
                .iter()
                .filter(|a| a["running"].as_bool() == Some(true))
                .count();
            if running > 0 {
                about.push(format!("{running} subagent(s) running"));
            }
            println!("    {}", about.join(" · "));
            if let Some(r) = x["reason"].as_str() {
                println!("    waiting: {r}");
            } else if let Some(d) = x["job"]["detail"].as_str() {
                println!("    job: {d}");
            }
            println!("    produced: {}", produced(&x["outputs"]));
            let containers = list(x, "containers");
            if !containers.is_empty() {
                println!(
                    "    + {} container(s), {:.0}% CPU, {}",
                    containers.len(),
                    num(x, "container_cpu"),
                    bytes(u(x, "container_mem"))
                );
            }
        }
    }
    for t in list(snap, "tooling") {
        println!(
            "\n{}: {} process(es), {:.0}% CPU, {}",
            s(t, "label"),
            u(t, "procs"),
            num(t, "cpu"),
            bytes(u(t, "rss"))
        );
    }
    let orphans = list(snap, "orphans");
    if !orphans.is_empty() {
        println!("\nOrphans (left behind by ended sessions)");
        for o in orphans {
            println!(
                "  {:<28} {:>6.0}% {:>9}  {} process(es), {} container(s), ended {} ago",
                truncate(s(o, "name"), 28),
                num(o, "cpu"),
                bytes(u(o, "rss")),
                u(o, "procs"),
                list(o, "containers").len(),
                duration(now.saturating_sub(u(o, "ended_at")))
            );
            for p in list(o, "processes").iter().take(5) {
                println!("      {:>7}  {}", p["pid"], s(p, "cmd"));
            }
        }
    }
    let apps = list(snap, "apps");
    let other = &snap["apps_other"];
    for (title, ui) in [("Apps", true), ("System & background", false)] {
        let rows: Vec<&Value> = apps
            .iter()
            .filter(|a| a["ui"].as_bool().unwrap_or(false) == ui)
            .collect();
        let hidden = if ui { 0 } else { u(other, "units") };
        if rows.is_empty() && hidden == 0 {
            continue;
        }
        println!("\n{title}");
        for a in rows {
            let note = match (u(a, "agents"), u(a, "agent_mem")) {
                (0, 0) => String::new(),
                (0, m) => format!("  + AI agents {} listed above", bytes(m)),
                (n, m) => format!("  + {n} AI agent(s), {} listed above", bytes(m)),
            };
            println!(
                "  {:<36} {:>6.0}% {:>9}{note}",
                truncate(s(a, "id"), 36),
                num(a, "cpu"),
                bytes(u(a, "mem"))
            );
        }
        if hidden > 0 {
            println!(
                "  {:<36} {:>6.0}% {:>9}",
                format!("{hidden} smaller unit(s)"),
                num(other, "cpu"),
                bytes(u(other, "mem"))
            );
        }
    }
    let containers = list(snap, "containers");
    if !containers.is_empty() {
        println!("\nContainers");
        for c in containers {
            let owner = if let Some(o) = c["orphan_of"].as_str() {
                format!("orphan of {o}")
            } else if let Some(k) = c["session"].as_str() {
                let name = list(snap, "agents")
                    .iter()
                    .find(|x| s(x, "key") == k)
                    .map(|x| s(x, "title"))
                    .unwrap_or(k);
                format!("agent {name}")
            } else {
                String::new()
            };
            println!(
                "  {:<36} {:>6.0}% {:>9}  {owner}",
                truncate(s(c, "name"), 36),
                num(c, "cpu"),
                bytes(u(c, "mem"))
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
        "\n  {:<10} {:<34} {:>8} {:>8} {:>9} {:>9} {:>7} {:>7}",
        "KIND", "NAME", "CPU AVG", "CPU PEAK", "RAM AVG", "RAM PEAK", "MINUTES", "TOKENS"
    );
    for r in list(h, "ranking") {
        let kind = match s(r, "kind") {
            "session" => "agent",
            k => k,
        };
        println!(
            "  {:<10} {:<34} {:>7.0}% {:>7.0}% {:>9} {:>9} {:>7} {:>7}",
            kind,
            truncate(s(r, "label"), 34),
            num(r, "cpu_avg"),
            num(r, "cpu_max"),
            bytes(num(r, "mem_avg") as u64),
            bytes(num(r, "mem_max") as u64),
            u(r, "minutes"),
            r["tokens"]
                .as_u64()
                .map(tokens)
                .unwrap_or_else(|| "-".into())
        );
    }
}

/// `317000` -> `317k`, `37700000` -> `37.7M`.
fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// What an agent produced, in a few words, as the menu shows it.
fn produced(o: &Value) -> String {
    let n = |k: &str| u(o, k);
    let plural =
        |count: u64, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if n("files") > 0 {
        parts.push(format!(
            "{} (+{} -{})",
            plural(n("files"), "file"),
            n("lines_added"),
            n("lines_removed")
        ));
    }
    if n("commits") > 0 {
        let pushed = if n("pushes") > 0 { ", pushed" } else { "" };
        parts.push(format!("{}{pushed}", plural(n("commits"), "commit")));
    } else if n("pushes") > 0 {
        parts.push("pushed".into());
    }
    for c in list(o, "changes") {
        let (kind, mark) = match s(c, "host") {
            "gitlab" => ("MR", "!"),
            "github" => ("PR", "#"),
            _ => ("PR", "#"),
        };
        let state = match s(c, "state") {
            "open" => String::new(),
            st => format!(" {st}"),
        };
        parts.push(format!("{kind} {mark}{}{state}", u(c, "number")));
    }
    match n("plans") {
        0 => {}
        1 => parts.push("plan".into()),
        k => parts.push(format!("{k} plans")),
    }
    for (key, word) in [
        ("docs", "doc"),
        ("published", "published page"),
        ("notes", "note"),
        ("deploys", "deploy"),
        ("releases", "release"),
        ("restores", "restore"),
        ("todos", "to-do"),
        ("issues", "issue"),
        ("messages", "message"),
        ("records", "record"),
    ] {
        if n(key) > 0 {
            parts.push(plural(n(key), word));
        }
    }
    if n("memory") > 0 {
        parts.push("memory".into());
    }
    if n("config") > 0 {
        parts.push("config".into());
    }
    if parts.is_empty() {
        "answer only".into()
    } else {
        parts.join(", ")
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
        let matches: Vec<&Value> = list(&snap, "agents")
            .iter()
            .filter(|x| [s(x, "name"), s(x, "title"), s(x, "key"), s(x, "id")].contains(&target))
            .collect();
        match matches.as_slice() {
            [one] => (
                "agent",
                s(one, "key").to_string(),
                format!("agent {} ({} processes)", s(one, "title"), u(one, "procs")),
            ),
            [] => return Err(format!("no agent named {target}").into()),
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
    if cmd == "orca-show" {
        let (Some(cli), Some(session)) = (args.get(1), args.get(2)) else {
            return Err("usage: hivetop orca-show <orca command> <session id>".into());
        };
        return Ok(hivetop::hosts::orca_show(cli, session)?);
    }
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
