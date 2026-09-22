#!/usr/bin/env bash
# Build Synapse for Linux from source and install it for the current user.
# Accepts the same options as the release installer:
#   --no-udev  --autostart  --prefix DIR
set -euo pipefail

for arg in "$@"; do
    case "$arg" in
        -h|--help) exec "$(dirname "${BASH_SOURCE[0]}")/../packaging/release/install.sh" --help ;;
    esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo was not found. Install Rust first (e.g. CachyOS/Arch: sudo pacman -S rust," >&2
    echo "Debian/Ubuntu: https://rustup.rs)." >&2
    exit 1
fi

echo "==> Building (release)..."
(cd "$ROOT" && cargo build --release --locked)

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
"$ROOT/scripts/package-release.sh" --stage "$STAGE/synapse-linux"
"$STAGE/synapse-linux/install.sh" "$@"
