#!/usr/bin/env bash
# Assemble the prebuilt release archive from a finished release build.
#
#   cargo build --release --locked
#   scripts/package-release.sh             -> dist/synapse-linux-<arch>.tar.gz + SHA256SUMS
#   scripts/package-release.sh --stage DIR -> only lay the files out in DIR
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAGE_ONLY=""
if [ "${1:-}" = "--stage" ]; then
    STAGE_ONLY="${2:?--stage needs a directory}"
fi

cd "$ROOT"
TARGET="${CARGO_TARGET_DIR:-target}"
ARCH="$(uname -m)"
NAME="synapse-linux-$ARCH"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"

for bin in synapse-linux synapsectl; do
    if [ ! -x "$TARGET/release/$bin" ]; then
        echo "missing $TARGET/release/$bin; run: cargo build --release --locked" >&2
        exit 1
    fi
done

if [ -n "$STAGE_ONLY" ]; then
    STAGE="$STAGE_ONLY"
else
    rm -rf dist
    STAGE="dist/$NAME"
fi
mkdir -p "$STAGE"

install -Dm755 "$TARGET/release/synapse-linux" "$STAGE/bin/synapse-linux"
install -Dm755 "$TARGET/release/synapsectl" "$STAGE/bin/synapsectl"
install -Dm644 packaging/udev/70-synapse-linux.rules "$STAGE/udev/70-synapse-linux.rules"
install -Dm644 packaging/synapse-linux.desktop "$STAGE/share/applications/synapse-linux.desktop"
install -Dm644 packaging/icons/synapse-linux.svg "$STAGE/share/icons/hicolor/scalable/apps/synapse-linux.svg"
install -Dm755 packaging/release/install.sh "$STAGE/install.sh"
install -Dm755 scripts/uninstall.sh "$STAGE/uninstall.sh"
install -Dm644 README.md README.tr.md LICENSE CHANGELOG.md -t "$STAGE"
install -Dm644 docs/PROTOCOL.md "$STAGE/PROTOCOL.md"
echo "$VERSION" > "$STAGE/VERSION"

if [ -n "$STAGE_ONLY" ]; then
    exit 0
fi

tar -C dist --owner=0 --group=0 -czf "dist/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"
(cd dist && sha256sum "$NAME.tar.gz" > SHA256SUMS)
echo "dist/$NAME.tar.gz (version $VERSION)"
