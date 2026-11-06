# hivetop: design notes

A lightweight resource monitor for the GNOME top bar. It groups usage the way
you work when several Claude Code sessions run in parallel: by **Claude
session**, by **app** and by **container**. A plain process list would not.

```
Top bar:   🐝 cpu 12%  ram 65%        (the bee turns yellow or red under CPU/memory/IO pressure)

Menu:      System: 12 cores, calm                      ~~~    12%    65%
           > Claude sessions (6)   2 busy, 4 idle             3.4%  3.5 GB
           > Left behind by ended sessions (1)                  0%   19 MB
           > Background Claude (2)                            0.3%  341 MB
           > Apps (12)                                         10%   12 GB
           > Docker (1)                                       0.2%   65 MB
           > History   last 24h: average CPU and RAM           11%  9.8 GB
```

Sections start collapsed every time the menu opens; click a header to expand
it. Expanding a session shows its processes (MCP servers dimmed as
infrastructure), the containers it started, and the actions **Go to <app>**
(raises the VSCodium or terminal window that hosts the session), **Scratchpad**
(opens the session's scratchpad folder), **Copy details** and **Kill session**.

CPU in the menu is a share of the whole machine, as in GNOME System Monitor:
on 12 cores, one busy core shows as 8.3%. The CLI and the notification
thresholds use percent of one core, like `top`.

## How it works

```
GNOME Shell extension (GJS)            hivetopd (Rust, systemd --user)
  top-bar label  <── Updated signal ──  samples every 5 s (2 s while the menu is open)
  menu           ── GetSnapshot ────>   /proc, /sys/fs/cgroup, /proc/pressure
  actions        ── Kill/SetActive ─>   ~/.claude/sessions/*.json, Docker socket
                                        SQLite history, desktop notifications
hivetop CLI      ── same D-Bus API ─>
```

GNOME on Wayland only lets Shell extensions draw in the top bar, so the
extension is a thin view. All sampling happens in the daemon, outside the
compositor. Measured on a 12-core laptop: the daemon uses about 6 MB RSS and
0.2% of one core with the menu closed, 0.5% while it is open; inside GNOME
Shell the extension costs about 1% of one core while the menu is open and
nothing measurable while it is closed.

- **Claude sessions**: a session is a `claude` process with no `claude`
  ancestor, plus all its descendants. Name, status (busy/idle) and session id
  come from `~/.claude/sessions/<pid>.json` when present. That file is an
  internal Claude Code format, so a session still shows without it, named
  after its working directory. Children started in the first 10 s are counted
  as infrastructure (MCP servers, plugins). The "now running" text is the
  newest other child, with the Bash tool wrapper stripped to the real command.
- **Apps**: the systemd scopes GNOME creates for each launched app
  (`app-gnome-codium-1234.scope`), read from cgroup v2 files, cheap and
  children included. Memory leaves out reclaimable page cache, like the
  system "used" figure. Two GNOME quirks are corrected:
  - GNOME moves an app into its scope only after spawning it, so whatever a
    browser forks in between stays in GNOME Shell's cgroup. That made Brave's
    renderers look like 4 GB of "GNOME Shell". Such processes are credited to
    their nearest app ancestor, and their share of memory (RssAnon +
    RssShmem) moves with them.
  - Portals open a second scope for the same app (`app-com.google.Chrome-<pid>`
    next to `app-gnome-google\x2dchrome-<pid>`). Both count as one app.
- **Containers**: listed through the Docker socket, measured through their
  cgroups. A container belongs to a session when the session's working
  directory is its compose `working_dir` or lies inside it (addon repos,
  worktrees).
- **Orphans**: processes and `docker compose run` containers still running 10 s
  after their session ended. The daemon remembers which session started what
  in memory only, so sessions that ended before it started are not tracked.
- **History**: per-minute average and peak for the system, every session, the
  top 5 apps and every container. Rows are merged into 10-minute rows after 7
  days and deleted after 30 days. Stored in `~/.local/share/hivetop/history.db`
  (about 10 MB).

## Requirements

- Linux with cgroup v2 and PSI (any distribution from the last few years).
- GNOME Shell 45 to 49. Only 46 has been tested so far.
- Rust toolchain (`rustup`) and a C compiler (SQLite is built in).
- Optional: membership in the `docker` group for container data.

## Install

```sh
./install.sh
```

It builds the release binaries, installs `hivetopd` and `hivetop` to
`~/.local/bin`, registers the systemd user unit and the D-Bus activation file,
installs the extension, and starts the daemon. On Wayland, GNOME loads a new or
updated extension only at login, so log out and back in once, then:

```sh
gnome-extensions enable hivetop@sirwilliamdev.github.com
```

Remove with `./install.sh --uninstall` (keeps config and history) or
`./install.sh --purge`.

## Configuration

The extension preferences (Extensions app, or **Settings** in the menu) control
the top-bar label and its position. Daemon thresholds, history and Docker
settings live in `~/.config/hivetop/config.toml`: see
[`dist/config.example.toml`](../dist/config.example.toml). The preferences page
can create and open that file.

Default notifications fire when:

- a session uses more than 4 GB, or more than 300% CPU for 60 s;
- memory pressure (PSI some, avg10) goes above 20%;
- an ended session leaves processes or containers running.

## CLI

```sh
hivetop status            # same data as the menu, as a table
hivetop status --json     # full snapshot
hivetop history --range 7d
hivetop kill acme-api-99  # SIGTERM the whole session tree, SIGKILL after 5 s
hivetop kill 12345        # one process
```

## Security notes

- `Kill` only accepts targets present in the latest snapshot, owned by your
  user, and never PID 1, `gnome-shell`, `systemd` or the daemon itself.
- Only containers attributed to a session, or left behind by one, can be
  stopped. Note that Docker socket access is equivalent to root.
- The daemon reads `~/.claude/sessions/*.json` only. It never opens the `.key`
  files next to them, and never reads process environments.

## Development

```sh
cargo test                                   # parsers, attribution, orphans, history
cargo build --release && ./target/release/hivetopd   # run against the live system
./target/release/hivetop status
```

Paths can be redirected for testing: `HIVETOP_PROC_ROOT`,
`HIVETOP_CGROUP_ROOT`, `HIVETOP_CLAUDE_SESSIONS`, plus `XDG_CONFIG_HOME` and
`XDG_DATA_HOME` for config and history.

To try the extension without logging out, `scripts/nested-test.sh` runs a
nested GNOME Shell with this checkout's extension and daemon, isolated from your
session, and drives it with steps (open the menu, expand rows, screenshots, CPU,
RSS and leak checks):

```sh
cargo build --release
scripts/nested-test.sh open expand:sessions row:sessions:0 wait:1 shot:session
scripts/nested-test.sh rss:start cycle:25 rss:after   # leak check
```

GNOME Shell keeps memory for every popup menu item that gets destroyed, so the
extension reuses rows and process lines by key instead of rebuilding them.
