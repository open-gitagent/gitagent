#!/bin/sh
# Installs the gitagent binary for this system from GitHub releases.
#   INSTALL_DIR  where to put it (default: ~/.local/bin)
#   VERSION      a release tag such as v0.1.0 (default: the latest release)
#   REPO         owner/name on GitHub (default: open-gitagent/gitagent)
#   BASE_URL     download from here instead of GitHub (a mirror)
set -eu

REPO="${REPO:-open-gitagent/gitagent}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${VERSION:-latest}"

os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-musl ;;
  Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
  Darwin/arm64) target=aarch64-apple-darwin ;;
  Darwin/x86_64) target=x86_64-apple-darwin ;;
  *) echo "gitagent: no prebuilt binary for $os/$arch; build from source with cargo" >&2; exit 1 ;;
esac

if [ -n "${BASE_URL:-}" ]; then
  base="$BASE_URL"
elif [ "$VERSION" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi
archive="gitagent-$target.tar.gz"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $archive from $base"
curl -fsSL -o "$tmp/$archive" "$base/$archive"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"

expected=$(grep " $archive\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$archive" | cut -d' ' -f1)
else
  actual=$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)
fi
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  echo "gitagent: checksum mismatch for $archive" >&2
  exit 1
fi

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/gitagent" "$INSTALL_DIR/gitagent"
echo "Installed $("$INSTALL_DIR/gitagent" --version) to $INSTALL_DIR/gitagent"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "Add $INSTALL_DIR to your PATH to run it as 'gitagent'." ;;
esac
