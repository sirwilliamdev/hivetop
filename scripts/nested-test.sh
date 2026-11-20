#!/usr/bin/env bash
# Runs a nested GNOME Shell (Wayland, unsafe mode) with this checkout's extension
# and daemon, isolated from your session: own D-Bus, settings and data dirs.
# Then drives it with steps. Needs `cargo build --release` first.
#
#   scripts/nested-test.sh [--size WxH] [--out DIR] STEP...
#
# Steps:
#   open | close              open or close the hivetop menu (AI Agents opens expanded)
#   tab:NAME                  switch tab: now, history
#   expand:SECTION            toggle a section: agents, apps, background (System & background, inside apps), stacks
#   row:SECTION:INDEX         open the INDEX-th row of a section (expand the section first)
#   hover:SECTION:INDEX       move a virtual pointer over that row
#   shot:NAME                 screenshot to OUT/NAME.png
#   shot-menu:NAME            screenshot of the top bar button and open menu only
#   wait:SECS                 pause
#   eval:JS                   run JS in the shell; `_ht` is the indicator
#   cpu:LABEL:SECS            nested gnome-shell CPU use over SECS
#   rss:LABEL                 nested gnome-shell RSS after a forced GC
#   actors:LABEL              number of actors on the stage
#   cycle:N                   N times: open, expand apps, switch to history and back, close
#
# Example:
#   scripts/nested-test.sh open row:agents:0 wait:1 shot:agent tab:history wait:2 shot:history
#
# HIVETOP_DAEMON=PATH runs PATH instead of target/release/hivetopd, such as
# scripts/demo-daemon.py for made-up data.
#
# Exits non-zero when the shell logged JavaScript errors.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
UUID="hivetop@sirwilliamdev.github.com"

if [[ -z "${HIVETOP_NESTED_INNER:-}" ]]; then
    size=1920x1080
    out=nested-out
    while [[ $# -gt 0 && "$1" == --* ]]; do
        case "$1" in
            --size) size="$2"; shift 2 ;;
            --out) out="$2"; shift 2 ;;
            *) echo "unknown option $1" >&2; exit 2 ;;
        esac
    done
    daemon="$(realpath -m "${HIVETOP_DAEMON:-$REPO/target/release/hivetopd}")"
    [[ -x "$daemon" ]] || { echo "$daemon missing: run cargo build --release first" >&2; exit 2; }
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    scheme="$(gsettings get org.gnome.desktop.interface color-scheme 2>/dev/null || echo "'default'")"
    # The nested bus must start with these set: it looks up D-Bus activation
    # files under XDG_DATA_HOME.
    HIVETOP_NESTED_INNER=1 HIVETOP_DAEMON="$daemon" HIVETOP_OUT="$(realpath -m "$out")" HIVETOP_SCHEME="$scheme" \
        XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data" GSETTINGS_BACKEND=keyfile \
        MUTTER_DEBUG_DUMMY_MODE_SPECS="$size" \
        dbus-run-session -- "$0" "$@"
    exit $?
fi

OUT="$HIVETOP_OUT"
mkdir -p "$OUT" "$XDG_DATA_HOME/gnome-shell/extensions/$UUID" "$XDG_DATA_HOME/dbus-1/services"
cp -r "$REPO/extension/$UUID/." "$XDG_DATA_HOME/gnome-shell/extensions/$UUID/"
glib-compile-schemas "$XDG_DATA_HOME/gnome-shell/extensions/$UUID/schemas"
printf '[D-BUS Service]\nName=io.github.sirwilliamdev.Hivetop\nExec=%s\n' "$HIVETOP_DAEMON" \
    > "$XDG_DATA_HOME/dbus-1/services/io.github.sirwilliamdev.Hivetop.service"
gsettings set org.gnome.shell enabled-extensions "['$UUID']"
gsettings set org.gnome.desktop.interface color-scheme "$HIVETOP_SCHEME"

gnome-shell --nested --wayland --unsafe-mode > "$OUT/shell.log" 2>&1 &
shell_pid=$!

ev() {
    gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
        --method org.gnome.Shell.Eval "$1"
}

for _ in $(seq 80); do
    ev 1 > /dev/null 2>&1 && break
    sleep 0.5
done
sleep 6
ev "globalThis._ht = Main.panel.statusArea['$UUID'];
    globalThis._vp = imports.gi.Clutter.get_default_backend().get_default_seat()
        .create_virtual_device(imports.gi.Clutter.InputDeviceType.POINTER_DEVICE); 'ready'" > /dev/null

row() { echo "[..._ht._$1.rows.values()][$2]"; }

for step in "$@"; do
    IFS=: read -r cmd a b <<< "$step"
    case "$cmd" in
        open) ev "_ht.menu.open(); 'open'" > /dev/null; sleep 4 ;;
        close) ev "_ht.menu.close(); 'closed'" > /dev/null; sleep 1 ;;
        tab) ev "_ht._tabs.select('$a', true); 'x'" > /dev/null; sleep 1.5 ;;
        expand) ev "_ht._$a.header.activate(null); 'x'" > /dev/null; sleep 1 ;;
        row) ev "$(row "$a" "$b").setSubmenuShown(true); 'x'" > /dev/null; sleep 1 ;;
        hover)
            ev "const r = $(row "$a" "$b"); const [x, y] = r.get_transformed_position();
                const [w, h] = r.get_transformed_size();
                _vp.notify_absolute_motion(global.get_current_time() * 1000, x + w / 2, y + h / 2); 'x'" > /dev/null
            sleep 1.5 ;;
        shot)
            gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell/Screenshot \
                --method org.gnome.Shell.Screenshot.Screenshot false false "$OUT/$a.png" > /dev/null
            echo "screenshot $OUT/$a.png" ;;
        shot-menu)
            read -r x y w h < <(ev "const m = 12; const box = [_ht, _ht.menu.actor].map(x => {
                    const [px, py] = x.get_transformed_position(); const [pw, ph] = x.get_transformed_size();
                    return [px, py, px + pw, py + ph]; });
                const x0 = Math.max(0, Math.min(...box.map(b => b[0])) - m), y0 = 0;
                const x1 = Math.min(global.stage.width, Math.max(...box.map(b => b[2])) + m);
                const y1 = Math.min(global.stage.height, Math.max(...box.map(b => b[3])) + m);
                [x0, y0, x1 - x0, y1 - y0].map(Math.round).join(' ')" | grep -oE '[0-9]+( [0-9]+){3}')
            gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell/Screenshot \
                --method org.gnome.Shell.Screenshot.ScreenshotArea "$x" "$y" "$w" "$h" false "$OUT/$a.png" > /dev/null
            echo "screenshot $OUT/$a.png (${w}x$h)" ;;
        wait) sleep "$a" ;;
        eval) echo "eval: $(ev "${step#eval:}")" ;;
        cpu)
            t0=$(awk '{print $14 + $15}' "/proc/$shell_pid/stat")
            sleep "$b"
            t1=$(awk '{print $14 + $15}' "/proc/$shell_pid/stat")
            echo "cpu $a: $(LC_ALL=C awk -v d=$((t1 - t0)) -v s="$b" 'BEGIN { printf "%.2f", d / s }')% of one core" ;;
        rss)
            ev "imports.system.gc(); 'gc'" > /dev/null
            sleep 1
            echo "rss $a: $(LC_ALL=C awk '/VmRSS/ {printf "%.1f MB", $2 / 1024}' "/proc/$shell_pid/status")" ;;
        actors)
            echo "actors $a: $(ev "let n = 0; const walk = x => { n++; x.get_children().forEach(walk); }; walk(global.stage); String(n)")" ;;
        cycle)
            for _ in $(seq "$a"); do
                ev "_ht.menu.open(); _ht._apps.header.activate(null); 'o'" > /dev/null
                sleep 2
                ev "_ht._tabs.select('history', true); 'h'" > /dev/null
                sleep 1
                ev "_ht._tabs.select('now', true); 'n'" > /dev/null
                sleep 1
                ev "_ht.menu.close(); 'c'" > /dev/null
                sleep 0.5
            done
            echo "cycled $a" ;;
        *) echo "unknown step $step" >&2 ;;
    esac
done

daemon_pid=$(gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
    --method org.freedesktop.DBus.GetConnectionUnixProcessID io.github.sirwilliamdev.Hivetop 2>/dev/null \
    | grep -oE '[0-9]+' | tail -1 || true)
kill "$shell_pid" 2>/dev/null || true
wait "$shell_pid" 2>/dev/null || true
[[ -n "$daemon_pid" ]] && kill "$daemon_pid" 2>/dev/null || true

errors=$(grep -cE "JS ERROR|CRITICAL.*Exception" "$OUT/shell.log" || true)
echo "shell log: $OUT/shell.log ($errors JavaScript errors)"
[[ "$errors" -eq 0 ]]
