#!/usr/bin/env bash
# Remove Synapse for Linux installed by install.sh (release archive or
# scripts/install.sh). Settings in ~/.config/synapse-linux are kept.
set -euo pipefail

PREFIX="$HOME/.local"
REMOVE_UDEV=0
while [ $# -gt 0 ]; do
    case "$1" in
        --udev) REMOVE_UDEV=1 ;;
        --prefix) PREFIX="${2:?--prefix needs a directory}"; shift ;;
        -h|--help) echo "Usage: scripts/uninstall.sh [--udev] [--prefix DIR]"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

case "$PREFIX" in
    /*) ;;
    *) PREFIX="$PWD/$PREFIX" ;;
esac
BIN="$PREFIX/bin"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"

# Turn off the PipeWire clean microphone (restores the default input).
if [ -x "$BIN/synapsectl" ]; then
    "$BIN/synapsectl" mic-clean off >/dev/null 2>&1 || true
fi
systemctl --user disable --now synapse-linux-mic.service >/dev/null 2>&1 || true
rm -f "$CONFIG_HOME/systemd/user/synapse-linux-mic.service"
systemctl --user daemon-reload >/dev/null 2>&1 || true

# Stop the tray icon.
pkill -x synapse-linux >/dev/null 2>&1 || true

rm -f "$BIN/synapse-linux" "$BIN/synapsectl" \
    "$PREFIX/share/applications/synapse-linux.desktop" \
    "$PREFIX/share/icons/hicolor/scalable/apps/synapse-linux.svg" \
    "$CONFIG_HOME/autostart/synapse-linux-tray.desktop"

if [ "$REMOVE_UDEV" = 1 ]; then
    sudo rm -f /etc/udev/rules.d/70-synapse-linux.rules
    sudo udevadm control --reload-rules
fi

# Last, as this script may itself live there.
rm -rf "$PREFIX/share/synapse-linux"

echo "Removed. Settings are kept in $CONFIG_HOME/synapse-linux."
