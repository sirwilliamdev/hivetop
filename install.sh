#!/usr/bin/env bash
# Builds and installs hivetop for the current user (no root needed).
#   ./install.sh               build, install daemon + extension, start daemon
#   ./install.sh --uninstall   remove everything except config and history
#   ./install.sh --purge       remove everything, including config and history
set -euo pipefail
cd "$(dirname "$0")"

UUID="hivetop@sirwilliamdev.github.com"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
BIN_DIR="${HIVETOP_BIN_DIR:-$HOME/.local/bin}"
UNIT_DIR="$CONFIG_HOME/systemd/user"
DBUS_DIR="$DATA_HOME/dbus-1/services"
EXT_DIR="$DATA_HOME/gnome-shell/extensions/$UUID"
DBUS_FILE="$DBUS_DIR/io.github.sirwilliamdev.Hivetop.service"

uninstall() {
    gnome-extensions disable "$UUID" 2>/dev/null || true
    systemctl --user disable --now hivetopd.service 2>/dev/null || true
    rm -f "$BIN_DIR/hivetopd" "$BIN_DIR/hivetop" "$UNIT_DIR/hivetopd.service" "$DBUS_FILE"
    rm -rf "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/hivetop"
    rm -rf "$EXT_DIR"
    systemctl --user daemon-reload
    if [[ "${1:-}" == "--purge" ]]; then
        rm -rf "$CONFIG_HOME/hivetop" "$DATA_HOME/hivetop"
        echo "hivetop removed, including config and history."
    else
        echo "hivetop removed. Config ($CONFIG_HOME/hivetop) and history ($DATA_HOME/hivetop) kept."
    fi
}

case "${1:-}" in
    --uninstall) uninstall; exit 0 ;;
    --purge) uninstall --purge; exit 0 ;;
    "") ;;
    *) sed -n '2,5p' "$0"; exit 1 ;;
esac

CARGO="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
"$CARGO" build --release

install -d "$BIN_DIR" "$UNIT_DIR" "$DBUS_DIR"
install -m755 target/release/hivetopd target/release/hivetop "$BIN_DIR/"
sed "s|@BINDIR@|$BIN_DIR|g" dist/hivetopd.service.in > "$UNIT_DIR/hivetopd.service"
sed "s|@BINDIR@|$BIN_DIR|g" dist/io.github.sirwilliamdev.Hivetop.service.in > "$DBUS_FILE"

rm -rf "$EXT_DIR"
install -d "$EXT_DIR"
cp -r "extension/$UUID/." "$EXT_DIR/"
glib-compile-schemas "$EXT_DIR/schemas"

systemctl --user daemon-reload
systemctl --user enable hivetopd.service
systemctl --user restart hivetopd.service

if gnome-extensions enable "$UUID" 2>/dev/null && gnome-extensions info "$UUID" 2>/dev/null | grep -q "State: ACTIVE"; then
    echo "hivetop installed and active."
else
    echo "hivetop installed. GNOME on Wayland loads new or updated extensions only at login:"
    echo "log out and back in, then run: gnome-extensions enable $UUID"
fi
