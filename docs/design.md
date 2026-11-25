# hivetop: design notes

A lightweight resource monitor for the GNOME top bar. It groups usage the way
you work when several AI coding agents run in parallel: by **agent session**
(Claude Code today), by **app** and by **container**. A plain process list
would not.

```
Top bar:   🐝 cpu 12%  ram 65%        (the bee turns yellow or red under CPU/memory/IO pressure)

Menu:      System: 12 cores, calm                                   ~~~    12%    65%
           [ Now ]  [ History ]
           v AI Agents (4)   1 busy, 2 need you, 1 idle · 1 left behind  2.8%  4.3 GB
               ● Add rate limiting to the public API                   0.3%  0.9 GB
                 acme-api · needs you 40s · ctx 319k · 539k tok
                 Waiting: dialog open
               ● Nightly dependency upgrades  [bg]                     0.1%  380 MB
                 acme-api · needs you 12m · 38.8k tok
                 Waiting: Should I push main and start Phase 1?
               ● Popup layout and AI providers                         2.1%  1.2 GB
                 hivetop · busy 2m · ctx 152k · 317k tok · 1 subagent
                 cargo test --release
               ● Left behind: acme-api-3e                                0%   19 MB
               > Background (3)   SDK sessions and tooling              0.3%  341 MB
           v Apps (7)   apps with an open window, AI agents counted above  7%  9.2 GB
               Brave Web Browser   34 processes                           0.2%  2.2 GB
               ...
             > System & background (63)   desktop session, services, ...  2%  1.4 GB
           > Docker (1)                                               0.2%   65 MB
```

The menu has two tabs. **Now** shows what runs: AI Agents (open every time
the menu opens), Apps and Docker (closed until clicked). Apps lists the apps
with an open window; everything else (GNOME Shell, session services, portals,
tray apps such as Solaar, an app closed to the tray) sits in the **System &
background** group after them, closed until clicked, its rows expandable as
any app row. **History** shows the
system chart for 1 h to 30 days and the top consumers over that range; it
loads when the tab opens.

An agent row has three lines:

1. The title: a name the user gave the session (`/rename`), else the agent's
   own title for it, else its short name (`hivetop-97`).
2. Project, state and how long it has been in it, context size, tokens used,
   running subagents. States are busy (green), idle (grey) and **needs you**
   (yellow): a permission dialog, a question, a blocked background job.
3. The command running now, or why the agent waits, or for a background job
   its own one-line status; when idle, what the session produced. Containers
   and detached processes are appended.

Rows are ordered: agents that need you, busy ones, idle ones, then one
orange row per ended session that left work running, then Background (SDK
sessions such as plugin observers, and each provider's tooling). A provider
tag shows on rows only when agents of two providers run.

Expanding an agent shows the actions **Go to <app>** (raises the VSCodium,
Orca or terminal window that hosts the session; for an Orca terminal agent it
also switches to its tab with `orca terminal switch`, read from the session's
`ORCA_TERMINAL_HANDLE`; for an Orca chat, `hivetop orca-show` asks Orca for
the chat's workspace and opens it, since Orca has no command to show one chat
and cannot open folder workspaces from its CLI), **Scratchpad**, **Copy
details** and
**Kill session** (for a background job: **Attach**, which copies `claude
attach <id>`, and **Stop job**); then model, permission mode, branch,
context, tokens, cache reads and outputs; then subagents in a tinted panel
(running first, then the last 3 finished); then containers and processes (MCP
servers dimmed as infrastructure).

**Tokens** are input + cache writes + output, for the session and its
subagents. Cache reads are shown apart and left out: every turn reads the
whole context again, so on a long session they are 60 times the rest.

**Outputs** are what a session produced, in six groups, or "answer only":

| Group | Counts | Read from |
|---|---|---|
| Code | files, lines +/-, commits, pushes | file edits, git operations Claude records |
| Change requests | PRs (GitHub `#N`), MRs (GitLab `!N`), merged or closed | Claude's PR link records, git operations |
| Documents | plans, docs (`.md`, `.rst`, `.txt`), published pages and PDFs | plan tool, edited paths, Artifact and docs tools |
| Knowledge | memory files, notes, `CLAUDE.md` and Claude config | edited paths, note tools |
| Operations | deploys, releases, database restores | shell commands |
| Tasks and messages | to-dos, issues, mails and messages, records in other systems | tool names, shell commands |

CPU in the menu is a share of the whole machine, as in GNOME System Monitor:
on 12 cores, one busy core shows as 8.3%. The CLI and the notification
thresholds use percent of one core, like `top`.

## How it works

```
GNOME Shell extension (GJS)            hivetopd (Rust, systemd --user)
  top-bar label  <── Updated signal ──  samples every 5 s (2 s while the menu is open)
  menu           ── GetSnapshot ────>   /proc, /sys/fs/cgroup, /proc/pressure
  actions        ── Kill/SetActive ─>   agent state files and transcripts, Docker socket
                                        SQLite history, desktop notifications
hivetop CLI      ── same D-Bus API ─>
```

GNOME on Wayland only lets Shell extensions draw in the top bar, so the
extension is a thin view. All sampling happens in the daemon, outside the
compositor. Measured on a 12-core laptop: the daemon uses about 7 MB RSS and
0.2% of one core with the menu closed, 0.5% while it is open; inside GNOME
Shell the extension costs about 2% of one core while the menu is open and
nothing measurable while it is closed.

- **Agent providers** (`src/agents/`): a provider knows one coding agent:
  which processes are its sessions, which are its own helpers, and what it
  writes on disk about each session. Process trees, memory, containers and
  orphans are generic, so another agent (Codex, opencode) needs only a new
  provider. Claude Code is the only one so far.
- **Agent sessions**: a session is a process the provider recognises, with no
  other session above it, plus all its descendants; an agent started as a
  tool by another stays in the session that started it. Children started in
  the first 10 s are counted as infrastructure (MCP servers, plugins). The
  "now running" text is the newest other child, with the Bash tool wrapper
  stripped to the real command. A background command whose shell exits gets
  reparented to `systemd --user` and leaves the tree; it stays in its session
  as "detached", so a dev server started by an agent is counted, and killed,
  with the session. Memory is RssAnon + RssShmem per process: plain RSS
  counts shared pages (the `claude` binary, libraries) once per process and
  overstates a group of them.
- **Claude Code**: a session is a `claude` process, or any process its
  session file names. All its files are internal, undocumented formats, so
  each one only enriches a session, which still shows without them:
  - `~/.claude/sessions/<pid>.json`: name, status (busy, idle, waiting and
    why), kind (interactive or background job).
  - `~/.claude/jobs/<id>/state.json`: a background job's state and its
    one-line status.
  - `~/.claude/projects/<dir>/<session id>.jsonl`, the transcript: title,
    model, permission mode, branch, token usage, and what tools did. It is
    read incrementally, 1 MB at a time, from where the last read stopped.
  - `<session id>/subagents/agent-*.meta.json` next to it: each subagent's
    type and task, plus its own transcript for its tokens and outputs.
    Subagents run inside the session's process, so they have no CPU or
    memory of their own.
  - `/tmp/claude-<uid>/<project>/<session id>/scratchpad`.

  `claude --bg` runs jobs under a supervisor (`claude daemon run`) and one
  terminal host per job (`claude bg-pty-host`). Both count as Claude Code
  tooling, so each job is its own session.
- **Apps**: the systemd scopes GNOME creates for each launched app
  (`app-gnome-codium-1234.scope`), read from cgroup v2 files, cheap and
  children included. Memory leaves out reclaimable page cache, like the
  system "used" figure. Processes listed in the AI Agents section (sessions,
  background, left behind) are taken out of their app, so each process is
  counted once: their RssAnon + RssShmem and CPU leave the app's figures, and
  the app row says how many agents it hosts. Two GNOME quirks are corrected:
  - GNOME moves an app into its scope only after spawning it, so whatever a
    browser forks in between stays in GNOME Shell's cgroup. That made Brave's
    renderers look like 4 GB of "GNOME Shell". Such processes are credited to
    their nearest app ancestor, and their share of memory (RssAnon +
    RssShmem) moves with them.
  - Portals open a second scope for the same app (`app-com.google.Chrome-<pid>`
    next to `app-gnome-google\x2dchrome-<pid>`). Both count as one app.
  - An app can leave GNOME Shell's cgroup for the user manager before its
    scope exists, and never get one (Sublime Text). When a launcher entry
    carries the process name, it becomes its own app with its children.

  The extension puts a unit under Apps when one of its windows is open
  (matched by app id or by PID; desktop icons and other windows kept off the
  taskbar do not count), and under System & background otherwise. The daemon
  cannot see windows, so for the CLI it guesses from desktop entries: an app
  has a launcher entry that is shown and is not autostarted (`ui` in the
  snapshot). It sends the 12 largest of each kind, and totals for the rest.
- **Containers**: listed through the Docker socket, measured through their
  cgroups. A container belongs to a session when the session's working
  directory is its compose `working_dir` or lies inside it (addon repos,
  worktrees).
- **Orphans**: processes and `docker compose run` containers still running 10 s
  after their session ended. Which session started what is saved to
  `$XDG_RUNTIME_DIR/hivetop/tracker.json`, so it survives a daemon restart
  (not a logout). Processes that left their session before the daemon first
  saw them cannot be attributed.
- **History**: per-minute average and peak for the system, every agent session
  (labelled with its title, plus its latest token total), the top 5 apps
  (their own usage, agents left out) and every container. Rows are merged
  into 10-minute rows after 7 days and deleted after 30 days. Stored in
  `~/.local/share/hivetop/history.db` (about 10 MB).

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

- an agent session uses more than 4 GB, or more than 300% CPU for 60 s;
- memory pressure (PSI some, avg10) goes above 20%;
- an ended session leaves processes or containers running.

With **Notifications > When an AI agent needs you** on in the extension
preferences (off by default), the extension also notifies once each time an
agent starts waiting; clicking the notification raises the agent's window.

## CLI

```sh
hivetop status            # same data as the menu, as a table
hivetop status --json     # full snapshot
hivetop history --range 7d
hivetop kill acme-api-99  # SIGTERM the whole session tree (by name, title or key), SIGKILL after 5 s
hivetop kill 12345        # one process
```

## Security notes

- `Kill` only accepts targets present in the latest snapshot, owned by your
  user, and never PID 1, `gnome-shell`, `systemd` or the daemon itself. Stop
  job runs the job's own `claude` binary as `claude stop <id>`, and
  terminates the job's processes if that fails.
- Only containers attributed to a session, or left behind by one, can be
  stopped. Note that Docker socket access is equivalent to root.
- From Claude Code's files the daemon keeps metadata only. Transcripts hold
  the whole conversation: hivetop parses only the lines it needs, keeps
  titles, model, mode, branch, token counts, file paths and git operation
  types, and never stores message text or prompts. Shell commands are matched
  against a few patterns (deploy, release, restore, issue create) and
  dropped. Job state files also hold environment variables, which the parser
  skips. It never opens the `.key` files next to the session files, never
  reads the background daemon's `roster.json` (it holds auth tokens), and
  never reads process environments.
- The menu never shows a prompt: what you typed can be on screen while you
  share it.

## Development

```sh
cargo test                                   # parsers, attribution, orphans, history
cargo build --release && ./target/release/hivetopd   # run against the live system
./target/release/hivetop status
```

Paths can be redirected for testing: `HIVETOP_PROC_ROOT`,
`HIVETOP_CGROUP_ROOT`, `HIVETOP_CLAUDE_SESSIONS` (its parent directory is then
read for `projects/` and `jobs/`), `HIVETOP_CLAUDE_TMP`, plus
`XDG_CONFIG_HOME` and `XDG_DATA_HOME` for config and history. Otherwise
`[agents.claude]` in the config sets Claude Code's directories.

To try the extension without logging out, `scripts/nested-test.sh` runs a
nested GNOME Shell with this checkout's extension and daemon, isolated from your
session, and drives it with steps (open the menu, expand rows, screenshots, CPU,
RSS and leak checks):

```sh
cargo build --release
scripts/nested-test.sh open row:agents:0 wait:1 shot:agent tab:history wait:2 shot:history
scripts/nested-test.sh cycle:5 actors:warm cycle:20 actors:after   # leak check
```

GNOME Shell keeps memory for every popup menu item that gets destroyed, so the
extension reuses rows and process lines by key instead of rebuilding them.
