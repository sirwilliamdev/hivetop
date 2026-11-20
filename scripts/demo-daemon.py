#!/usr/bin/env python3
"""Stand-in for hivetopd that serves made-up data, for screenshots.

It owns the same D-Bus name and answers the same methods as the real daemon,
so the extension cannot tell them apart. Nothing on this machine is read.
Run it inside a nested shell: scripts/screenshots.sh starts it there.
"""

import json
import math
import random
import time

from gi.repository import Gio, GLib

BUS = "io.github.sirwilliamdev.Hivetop"
PATH = "/io/github/sirwilliamdev/Hivetop"
XML = f"""
<node>
  <interface name="{BUS}">
    <method name="GetSnapshot">
      <arg type="b" direction="in"/><arg type="s" direction="out"/>
    </method>
    <method name="SetActive"><arg type="b" direction="in"/></method>
    <method name="GetHistory">
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
      <arg type="s" direction="out"/>
    </method>
    <method name="Kill">
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
      <arg type="b" direction="out"/>
    </method>
    <signal name="Updated"><arg type="s"/></signal>
  </interface>
</node>
"""

GB = 1024**3
MB = 1024**2
NCPU = 16
MEM_TOTAL = 32 * GB
HOME = GLib.get_home_dir()


def wave(seed, base, amp, n=60, step=10, now=None):
    """`n` points ending now: [ts, value] with a smooth random wobble."""
    rnd = random.Random(seed)
    now = now or int(time.time())
    phase = rnd.random() * 6
    return [
        (now - (n - 1 - i) * step, max(0.0, base + amp * math.sin(phase + i / 6) + rnd.uniform(-amp, amp) / 3))
        for i in range(n)
    ]


def spark(seed, cpu, mem, cpu_amp=None, mem_amp=None):
    cpus = wave(seed, cpu, cpu_amp if cpu_amp is not None else cpu / 2)
    mems = wave(seed + 1, mem, mem_amp if mem_amp is not None else mem / 20)
    return [[t, round(c, 1), round(m)] for (t, c), (_, m) in zip(cpus, mems)]


def proc(pid, comm, cmd, rss, cpu, infra=False):
    return {"pid": pid, "start": 0, "comm": comm, "cmd": cmd, "rss": rss, "cpu": cpu, "infra": infra}


def outputs(**kw):
    o = {
        "files": 0, "lines_added": 0, "lines_removed": 0, "commits": 0, "pushes": 0,
        "changes": [], "plans": 0, "docs": 0, "published": 0, "memory": 0, "notes": 0,
        "config": 0, "deploys": 0, "releases": 0, "restores": 0, "todos": 0,
        "issues": 0, "messages": 0, "records": 0,
    }
    o.update(kw)
    return o


def tokens(total):
    out = total // 12
    write = total // 4
    return {"total": total, "input": total - out - write, "output": out, "cache_write": write, "cache_read": total * 9}


def agent(n, pid, title, project, state, since, rss, cpu, ctx, tok, **kw):
    a = {
        "key": f"{pid}:{1000 + n}",
        "id": f"5f3c9a1e-0000-4000-8000-00000000000{n}",
        "provider": "claude",
        "kind": "interactive",
        "pid": pid,
        "name": f"{project}-{pid % 100}",
        "title": title,
        "project": project,
        "cwd": f"{HOME}/code/{project}",
        "state": state,
        "state_since": int(time.time()) - since,
        "reason": None,
        "job": None,
        "model": "opus 5.5",
        "mode": "auto",
        "branch": "main",
        "context": ctx,
        "tokens": tokens(tok),
        "outputs": outputs(),
        "subagents": [],
        "started_at": int(time.time()) - since - 1800,
        "app": "org.gnome.Terminal",
        "procs": 4,
        "detached": 0,
        "rss": rss,
        "cpu": cpu,
        "activity": None,
        "scratchpad": None,
        "containers": [],
        "container_mem": 0,
        "container_cpu": 0.0,
        "ancestors": [],
        "spark": spark(n, cpu, rss),
        "processes": [
            proc(pid, "claude", "claude", int(rss * 0.7), cpu * 0.4, infra=True),
            proc(pid + 40, "node", "node mcp-server.js", int(rss * 0.2), 0.1, infra=True),
        ],
    }
    a.update(kw)
    return a


def subagent(i, kind, description, running, age, tok):
    now = int(time.time())
    return {
        "id": f"a{i}", "kind": kind, "description": description, "running": running,
        "started_at": now - age, "ended_at": None if running else now - age // 3, "tokens": tok,
    }


def container(cid, name, image, project, service, mem, cpu, session=None):
    return {
        "id": cid, "name": name, "image": image, "project": project, "service": service,
        "working_dir": f"{HOME}/code/{project}", "oneoff": False, "mem": mem, "cpu": cpu,
        "session": session, "orphan_of": None,
    }


def app(app_id, kind, mem, cpu, procs, pid, agents=0, agent_mem=0, agent_cpu=0.0):
    return {
        "id": app_id, "kind": kind, "ui": kind == "app", "mem": mem, "cpu": cpu,
        "agents": agents, "agent_mem": agent_mem, "agent_cpu": agent_cpu,
        "procs": procs, "adopted": 0, "pids": [pid],
        "processes": [proc(pid, app_id.split(".")[-1].lower()[:15], app_id, mem, cpu)],
    }


def snapshot(detail):
    now = int(time.time())
    agents = [
        agent(
            1, 4211, "Add rate limiting to the public API", "acme-api", "waiting", 40,
            820 * MB, 0.4, 182_000, 412_000, branch="feat/rate-limit",
            reason="Bash: git push origin feat/rate-limit",
        ),
        agent(
            2, 5120, "Nightly dependency upgrades", "acme-api", "waiting", 12 * 60,
            380 * MB, 0.1, 64_000, 96_400, kind="job", app=None,
            reason="Open the PR for the 12 updated crates?",
            job={"id": "k3f9a2", "state": "blocked", "detail": "Open the PR for the 12 updated crates?"},
        ),
        agent(
            3, 6033, "Fix flaky checkout tests", "webshop", "busy", 3 * 60,
            1290 * MB, 64.5, 96_000, 231_000, branch="fix/checkout-flake", procs=9,
            activity="npm test -- checkout.spec.ts", containers=["c0ffee01"],
            container_mem=210 * MB, container_cpu=6.2,
            subagents=[subagent(1, "Explore", "Find every caller of applyCoupon", True, 70, 18_400)],
        ),
        agent(
            4, 7302, "Migrate login to OAuth 2.1", "identity", "busy", 9 * 60,
            1540 * MB, 186.0, 141_000, 388_000, branch="feat/oauth21", procs=14,
            activity="cargo test --release", app="code",
            subagents=[
                subagent(2, "general-purpose", "Port the token refresh flow", True, 240, 52_000),
                subagent(3, "Explore", "List endpoints that read the session cookie", False, 600, 21_000),
            ],
        ),
        agent(
            5, 3388, "Write the onboarding guide", "handbook", "idle", 14 * 60,
            410 * MB, 0.0, 58_000, 122_000, branch="docs/onboarding",
            outputs=outputs(
                files=3, lines_added=214, lines_removed=18, commits=1, pushes=1,
                changes=[{"number": 42, "url": "https://github.com/example/handbook/pull/42", "host": "github", "state": "open"}],
            ),
        ),
    ]
    agent_rss = sum(a["rss"] for a in agents)
    agent_cpu = sum(a["cpu"] for a in agents)
    containers = [
        container("c0ffee01", "webshop-test-db", "postgres:16", "webshop", "test-db", 210 * MB, 6.2, session=agents[2]["key"]),
        container("beef0001", "shop-db-1", "postgres:16", "shop", "db", 182 * MB, 0.8),
        container("beef0002", "shop-redis-1", "redis:7", "shop", "redis", 24 * MB, 0.3),
        container("beef0003", "shop-web-1", "node:22", "shop", "web", 145 * MB, 1.1),
    ]
    apps = [
        app("org.gnome.Terminal", "app", 310 * MB, 2.1, 6, 2001, agents=3, agent_mem=agent_rss, agent_cpu=agent_cpu),
        app("org.gnome.Nautilus", "app", 168 * MB, 0.3, 2, 2002),
        app("org.gnome.TextEditor", "app", 96 * MB, 0.1, 1, 2003),
        app("org.gnome.Calculator", "app", 61 * MB, 0.0, 1, 2004),
        app("org.gnome.Shell", "session", 540 * MB, 4.2, 3, 1500),
        app("tracker-miner-fs-3", "background", 31 * MB, 0.1, 2, 1600),
        app("dbus", "session", 27 * MB, 0.2, 7, 1601),
        app("xdg-desktop-portal-gnome", "app", 13 * MB, 0.0, 1, 1602),
    ]
    sys_cpu = (agent_cpu + 14) / NCPU
    sys_mem = 11.4 * GB
    s = {
        "ts": now,
        "ncpu": NCPU,
        "detail": detail,
        "system": {
            "cpu": round(sys_cpu, 1), "mem_total": MEM_TOTAL, "mem_used": int(sys_mem),
            "swap_total": 8 * GB, "swap_used": 0,
            "psi": {"cpu_some": 3.1, "memory_some": 0.0, "memory_full": 0.0, "io_some": 0.4, "io_full": 0.0},
            "pressure": 0,
            "spark": spark(0, sys_cpu, 100 * sys_mem / MEM_TOTAL, 4, 1),
        },
        "agents": agents,
        "tooling": [{
            "provider": "claude", "label": "Claude Code tooling", "procs": 3, "rss": 142 * MB, "cpu": 0.2,
            "processes": [proc(3001, "node", "node worker-service.cjs", 120 * MB, 0.2)],
        }],
        "orphans": [{
            "session_id": "9d1e", "name": "webshop-71", "cwd": f"{HOME}/code/webshop",
            "ended_at": now - 26 * 60, "procs": 1, "rss": 19 * MB, "cpu": 0.0, "containers": [],
            "processes": [proc(6801, "vite", "node node_modules/.bin/vite --port 5173", 19 * MB, 0.0)],
        }],
        "apps": apps,
        "apps_other": {"units": 38, "mem": 152 * MB, "cpu": 0.5},
        "containers": containers,
        "docker_ok": True,
    }
    if not detail:
        s["system"]["spark"] = []
        for x in [*s["agents"], *s["tooling"], *s["orphans"], *s["apps"]]:
            x["processes"] = []
        for x in s["agents"]:
            x["spark"] = []
    return s


def summary():
    s = snapshot(False)
    waiting = [
        {"key": a["key"], "title": a["title"], "reason": a["reason"], "since": a["state_since"]}
        for a in s["agents"] if a["state"] == "waiting"
    ]
    return {
        "ts": s["ts"], "cpu": s["system"]["cpu"], "mem_used": s["system"]["mem_used"],
        "mem_total": s["system"]["mem_total"], "pressure": 0, "agents": len(s["agents"]),
        "waiting": waiting, "orphans": len(s["orphans"]),
    }


RANGES = {"1h": 3600, "24h": 86400, "7d": 7 * 86400, "30d": 30 * 86400}


def history(rng):
    span = RANGES.get(rng, 86400)
    now = int(time.time())
    step = max(60, span // 120)
    rnd = random.Random(span)
    points = []
    for t in range(now - span, now, step):
        hour = (t % 86400) / 3600
        work = max(0.0, math.sin((hour - 7) / 12 * math.pi))
        cpu = 4 + 26 * work + rnd.uniform(0, 6)
        mem = (6 + 6 * work + rnd.uniform(0, 0.8)) * GB
        points.append([t, round(cpu, 1), round(min(100, cpu * 2.4), 1), mem, mem * 1.12])
    rows = [
        ("session", "Migrate login to OAuth 2.1", 41.0, 210.0, 1.3 * GB, 2.1 * GB, 412, 1_210_000),
        ("app", "google-chrome", 6.1, 88.0, 1.6 * GB, 2.4 * GB, 1380, None),
        ("session", "Fix flaky checkout tests", 22.4, 160.0, 0.9 * GB, 1.6 * GB, 236, 640_000),
        ("container", "shop-db-1", 1.2, 35.0, 180 * MB, 260 * MB, 1440, None),
        ("app", "org.gnome.Terminal", 2.3, 19.0, 300 * MB, 420 * MB, 1440, None),
        ("session", "Add rate limiting to the public API", 3.8, 95.0, 620 * MB, 880 * MB, 148, 412_000),
    ]
    ranking = [
        {"kind": k, "key": label, "label": label, "cpu_avg": ca, "cpu_max": cm, "mem_avg": ma,
         "mem_max": mm, "minutes": min(mins, span // 60), "tokens": tok}
        for k, label, ca, cm, ma, mm, mins, tok in rows
    ]
    return {
        "range": rng, "from": now - span, "to": now, "step": step,
        "target": {"kind": "system", "key": "system", "label": None},
        "points": points, "ranking": ranking,
    }


def on_call(conn, sender, path, iface, method, params, invocation):
    args = params.unpack()
    if method == "GetSnapshot":
        invocation.return_value(GLib.Variant("(s)", (json.dumps(snapshot(args[0])),)))
    elif method == "GetHistory":
        invocation.return_value(GLib.Variant("(s)", (json.dumps(history(args[0])),)))
    elif method == "Kill":
        invocation.return_value(GLib.Variant("(b)", (False,)))
    else:
        invocation.return_value(None)


def on_bus(conn, _name):
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    conn.register_object(PATH, node.interfaces[0], on_call, None, None)

    def tick():
        conn.emit_signal(None, PATH, BUS, "Updated", GLib.Variant("(s)", (json.dumps(summary()),)))
        return GLib.SOURCE_CONTINUE

    GLib.timeout_add_seconds(2, tick)


def main():
    loop = GLib.MainLoop()
    Gio.bus_own_name(Gio.BusType.SESSION, BUS, Gio.BusNameOwnerFlags.NONE, on_bus, None, lambda *_: loop.quit())
    loop.run()


if __name__ == "__main__":
    main()
