<p align="center"><img src="assets/logo.svg" width="200" alt="hivetop: a bee flying to its hive"></p>

# hivetop

A GNOME top-bar resource monitor that groups CPU and memory by Claude Code
session, app and Docker container.

## Install

Needs GNOME Shell 45+, cgroup v2 and a Rust toolchain.

```sh
./install.sh             # build, install, start the daemon
```

Log out and back in once: on Wayland, GNOME loads new extensions only at login.
Remove with `./install.sh --uninstall`.

## Use

- **Top bar**: `cpu` and `ram` usage. The bee turns yellow or red under CPU,
  memory or IO pressure.
- **Menu**: Claude sessions, processes left behind by ended sessions, apps,
  Docker, and 30 days of history.
- **CLI**: `hivetop status`, `hivetop history --range 7d`, `hivetop kill <session|pid>`.
- **Config**: `~/.config/hivetop/config.toml`, see [`dist/config.example.toml`](dist/config.example.toml).

Design notes: [`docs/design.md`](docs/design.md).
