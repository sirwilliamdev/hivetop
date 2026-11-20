#!/usr/bin/env bash
# Takes the README screenshots in a nested shell fed with made-up data
# (scripts/demo-daemon.py), so nothing from this machine shows up.
#
#   scripts/screenshots.sh [OUT]      default OUT: docs/screenshots
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$REPO/docs/screenshots}"

# The nested shell shows no app windows: make the demo apps (PIDs 2001-2004 in
# demo-daemon.py) look like they have one, so they list under Apps.
fake_windows="const pids = [2001, 2002, 2003, 2004];
    global.display.list_all_windows = () => pids.map(pid => ({
        is_skip_taskbar: () => false,
        get_window_type: () => imports.gi.Meta.WindowType.NORMAL,
        get_pid: () => pid,
    }));
    imports.gi.Shell.WindowTracker.get_default().get_window_app = () => null; 'ok'"

HIVETOP_DAEMON="$REPO/scripts/demo-daemon.py" "$REPO/scripts/nested-test.sh" --size 1920x1600 --out "$out" \
    "eval:$fake_windows" open expand:apps wait:1 shot-menu:now \
    tab:history wait:2 shot-menu:history
rm -f "$out/shell.log"
