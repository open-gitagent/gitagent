#!/usr/bin/env bash
# Builds the static Linux binaries used by the Harbor agent.
# Uses cargo-zigbuild when available (needed when cross-compiling from macOS).
set -euo pipefail
cd "$(dirname "$0")/.."

targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
  targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
fi

if command -v cargo-zigbuild >/dev/null 2>&1; then
  build=(cargo zigbuild)
else
  build=(cargo build)
fi

for target in "${targets[@]}"; do
  "${build[@]}" --release --locked --target "$target" -p gitagent-cli
  echo "built target/$target/release/gitagent"
done
