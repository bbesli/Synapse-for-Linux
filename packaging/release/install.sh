#!/usr/bin/env bash
# Install Synapse for Linux for the current user from this directory
# (an extracted release archive, or the staging folder made by
# scripts/install.sh when building from source).
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: ./install.sh [--no-udev] [--autostart] [--prefix DIR]

  --no-udev     skip the udev rule (it needs sudo; without it the app cannot
                open the device, but the app can install it later itself)
  --autostart   start the tray icon automatically at login
  --prefix DIR  install location (default: ~/.local)
EOF
}

PREFIX="$HOME/.local"
UDEV=1
AUTOSTART=0
while [ $# -gt 0 ]; do
    case "$1" in
        --no-udev) UDEV=0 ;;
        --autostart) AUTOSTART=1 ;;
        --prefix) PREFIX="${2:?--prefix needs a directory}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

# Resolve a relative --prefix against the caller's directory.
case "$PREFIX" in
    /*) ;;
    *) mkdir -p "$PREFIX" && PREFIX="$(cd "$PREFIX" && pwd)" ;;
esac
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for file in bin/synapse-linux bin/synapsectl udev/70-synapse-linux.rules \
    share/applications/synapse-linux.desktop share/icons/hicolor/scalable/apps/synapse-linux.svg; do
    if [ ! -e "$HERE/$file" ]; then
        echo "missing $file next to install.sh; run it from the extracted release folder" >&2
        exit 1
    fi
done

BIN="$PREFIX/bin"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/scalable/apps"
DATA="$PREFIX/share/synapse-linux"

echo "==> Installing Synapse for Linux to $PREFIX"
install -Dm755 "$HERE/bin/synapse-linux" "$BIN/synapse-linux"
install -Dm755 "$HERE/bin/synapsectl" "$BIN/synapsectl"
install -Dm644 "$HERE/share/icons/hicolor/scalable/apps/synapse-linux.svg" "$ICONS/synapse-linux.svg"
mkdir -p "$APPS"
sed "s|^Exec=.*|Exec=\"$BIN/synapse-linux\"|" "$HERE/share/applications/synapse-linux.desktop" > "$APPS/synapse-linux.desktop"
chmod 644 "$APPS/synapse-linux.desktop"
install -Dm644 "$HERE/udev/70-synapse-linux.rules" "$DATA/70-synapse-linux.rules"
if [ -f "$HERE/uninstall.sh" ]; then
    install -Dm755 "$HERE/uninstall.sh" "$DATA/uninstall.sh"
fi
{
    update-desktop-database "$APPS" || true
    gtk-update-icon-cache -q -t -f "$PREFIX/share/icons/hicolor" || true
    kbuildsycoca6 || kbuildsycoca5 || true
} >/dev/null 2>&1

if [ "$UDEV" = 1 ]; then
    RULE=/etc/udev/rules.d/70-synapse-linux.rules
    if cmp -s "$HERE/udev/70-synapse-linux.rules" "$RULE"; then
        echo "==> udev rule already installed ($RULE)"
    elif [ -e /usr/lib/udev/rules.d/70-synapse-linux.rules ]; then
        echo "==> udev rule provided by a system package"
    else
        echo "==> Installing the udev rule to $RULE (sudo asks for your password)..."
        sudo install -Dm644 "$HERE/udev/70-synapse-linux.rules" "$RULE"
        sudo udevadm control --reload-rules
        sudo udevadm trigger --subsystem-match=hidraw
        sudo udevadm settle || true
    fi
fi

if [ "$AUTOSTART" = 1 ]; then
    mkdir -p "$CONFIG_HOME/autostart"
    cat > "$CONFIG_HOME/autostart/synapse-linux-tray.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Synapse for Linux (tray)
Comment=Battery status and quick settings for Razer devices
Exec="$BIN/synapse-linux" --tray
Icon=synapse-linux
Terminal=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
X-KDE-autostart-after=panel
EOF
    echo "==> The tray icon will start at login"
fi

echo
echo "Done. Start \"Synapse for Linux\" from the application menu, or run:"
echo "  $BIN/synapse-linux          (window)"
echo "  $BIN/synapse-linux --tray   (tray icon)"
echo "  $BIN/synapsectl status      (command line)"
echo "Uninstall later with: $DATA/uninstall.sh"
case ":$PATH:" in
    *":$BIN:"*) ;;
    *) echo; echo "Note: $BIN is not in your PATH; add it to use synapsectl by name." ;;
esac
