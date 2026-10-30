#!/usr/bin/env bash
# Builds release archives into dist/: gitagent-<target>.tar.gz for each
# target that can be built on this machine, plus SHA256SUMS.
# Linux targets need cargo-zigbuild when built on macOS; macOS targets need a Mac.
set -euo pipefail
cd "$(dirname "$0")/.."

targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
  targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
  if [ "$(uname -s)" = Darwin ]; then
    targets+=(aarch64-apple-darwin x86_64-apple-darwin)
  fi
fi

rm -rf dist
mkdir -p dist
for target in "${targets[@]}"; do
  rustup target add "$target" >/dev/null 2>&1 || true
  case "$target" in
    *-linux-*) scripts/build-release.sh "$target" ;;
    *) cargo build --release --locked --target "$target" -p gitagent-cli ;;
  esac
  stage=$(mktemp -d)
  cp "target/$target/release/gitagent" "$stage/gitagent"
  cp README.md "$stage/"
  tar -czf "dist/gitagent-$target.tar.gz" -C "$stage" gitagent README.md
  rm -rf "$stage"
  echo "packed dist/gitagent-$target.tar.gz"
done

cd dist
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum gitagent-*.tar.gz > SHA256SUMS
else
  shasum -a 256 gitagent-*.tar.gz > SHA256SUMS
fi
cat SHA256SUMS
