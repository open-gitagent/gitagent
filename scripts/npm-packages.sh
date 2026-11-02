#!/usr/bin/env bash
# Builds the npm platform packages from the release archives in dist/: one
# package per platform in npm/<name>/, with the binary in bin/gitagent. The
# main package is clients/typescript; this checks that its version and its
# optional dependencies match the release version.
# Usage: scripts/npm-packages.sh <version or tag, e.g. v3.0.0>
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:?usage: scripts/npm-packages.sh <version>}"
version="${version#v}"

node - "$version" <<'JS'
const fs = require("fs");
const version = process.argv[2];
const pkg = JSON.parse(fs.readFileSync("clients/typescript/package.json", "utf8"));
const wrong = [];
if (pkg.version !== version) wrong.push(`version is ${pkg.version}`);
for (const [name, v] of Object.entries(pkg.optionalDependencies ?? {})) {
  if (v !== version) wrong.push(`${name} is ${v}`);
}
if (wrong.length) {
  console.error(`clients/typescript/package.json does not match ${version}: ${wrong.join(", ")}`);
  process.exit(1);
}
JS

rm -rf npm
mkdir -p npm
while read -r target os cpu; do
  archive="dist/gitagent-$target.tar.gz"
  if [ ! -f "$archive" ]; then
    echo "missing $archive" >&2
    exit 1
  fi
  name="gitagent-$os-$cpu"
  dir="npm/$name"
  mkdir -p "$dir/bin"
  tar -xzf "$archive" -C "$dir/bin" gitagent
  chmod 755 "$dir/bin/gitagent"
  cp LICENSE "$dir/"
  cat > "$dir/package.json" <<JSON
{
  "name": "@open-gitagent/$name",
  "version": "$version",
  "description": "The gitagent binary for $os $cpu. Install @open-gitagent/gitagent instead; it picks this package for you.",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/open-gitagent/gitagent.git"
  },
  "os": ["$os"],
  "cpu": ["$cpu"],
  "files": ["bin", "LICENSE"]
}
JSON
  echo "packed $dir"
done <<'TARGETS'
x86_64-unknown-linux-musl linux x64
aarch64-unknown-linux-musl linux arm64
aarch64-apple-darwin darwin arm64
x86_64-apple-darwin darwin x64
TARGETS
