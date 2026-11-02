#!/usr/bin/env bash
# Installs the npm packages the way users will, from a private local
# registry, and checks the result:
#   1. upgrading a global 2.x install (from npmjs) to this version,
#   2. the global `gitagent` command running a task with the binary npm installed,
#   3. the SDK as a project dependency, 2.x imports included.
# Needs: node and npm; a gitagent binary for this machine (the first argument).
# Runs on Linux x86_64 (the platform package it installs is linux-x64).
# Usage: scripts/test-npm-install.sh <path to a gitagent binary>
set -euo pipefail
cd "$(dirname "$0")/.."
binary=$(realpath "${1:?usage: scripts/test-npm-install.sh <gitagent binary>}")
version=$(node -p "require('./clients/typescript/package.json').version")
work=$(mktemp -d)
registry=http://localhost:4873
trap 'kill "${registry_pid:-}" 2>/dev/null || true; rm -rf "$work"' EXIT

echo "== the release archives, from $binary"
rm -rf dist && mkdir -p dist
stage=$(mktemp -d)
cp "$binary" "$stage/gitagent" && cp README.md "$stage/"
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin; do
  # Only linux-x64 gets installed here; the others need an archive to be packed.
  tar -czf "dist/gitagent-$target.tar.gz" -C "$stage" gitagent README.md
done
rm -rf "$stage"
scripts/npm-packages.sh "v$version"

echo "== a private registry"
cat > "$work/verdaccio.yaml" <<YAML
storage: $work/storage
auth:
  htpasswd:
    file: $work/htpasswd
uplinks:
  npmjs:
    url: https://registry.npmjs.org/
packages:
  "@open-gitagent/*":
    access: \$all
    publish: \$authenticated
  "**":
    access: \$all
    proxy: npmjs
log: { type: stdout, format: pretty, level: warn }
YAML
npx --yes verdaccio@6 --config "$work/verdaccio.yaml" --listen 4873 > "$work/verdaccio.log" 2>&1 &
registry_pid=$!
for _ in $(seq 60); do curl -sf "$registry/-/ping" >/dev/null && break; sleep 1; done
curl -sf "$registry/-/ping" >/dev/null || { cat "$work/verdaccio.log"; exit 1; }
token=$(curl -sf -X PUT -H 'content-type: application/json' \
  -d '{"name":"ci","password":"ci-password-1"}' "$registry/-/user/org.couchdb.user:ci" |
  node -p 'JSON.parse(require("fs").readFileSync(0, "utf8")).token')
export NPM_CONFIG_USERCONFIG="$work/npmrc"
echo "//localhost:4873/:_authToken=$token" > "$NPM_CONFIG_USERCONFIG"

echo "== publish, as the release does"
for dir in npm/*/; do npm publish "./${dir%/}" --registry "$registry" >/dev/null; done
(cd clients/typescript && npm install --no-audit --no-fund --omit=optional >/dev/null && npm publish --registry "$registry" >/dev/null)

agent="$work/agent"
mkdir -p "$agent"
printf 'spec_version: "0.1.0"\nname: lyra\nversion: 1.0.0\ndescription: t\n' > "$agent/agent.yaml"
echo "I am Lyra." > "$agent/SOUL.md"
# A task run ends with task_complete; an SDK conversation turn with a reply.
printf -- '- tool_calls: [{name: task_complete, arguments: {summary: "hello from the npm binary"}}]\n' > "$work/task.yaml"
printf -- '- text: "hello from the npm binary"\n' > "$work/script.yaml"
export HOME="$work/home" && mkdir -p "$HOME"
unset GITAGENT_BIN

echo "== upgrade a global 2.x install"
prefix="$work/global"
npm install -g --prefix "$prefix" "@open-gitagent/gitagent@2" --no-audit --no-fund >/dev/null
echo "2.x installed: $(npm ls -g --prefix "$prefix" --depth 0 | grep '@open-gitagent/gitagent@')"
npm install -g --prefix "$prefix" "@open-gitagent/gitagent@$version" --registry "$registry" --no-audit --no-fund >/dev/null
"$prefix/bin/gitagent" --version | tee "$work/version.txt"
grep -q "$version" "$work/version.txt"
out=$("$prefix/bin/gitagent" -p "say hello" -d "$agent" -m "scripted:$work/task.yaml" --out-dir "$work/out-cli")
echo "$out"
grep -q "hello from the npm binary" <<<"$out"

echo "== the SDK as a project dependency"
project="$work/project"
mkdir -p "$project" && cd "$project"
npm init -y >/dev/null
npm install "@open-gitagent/gitagent@$version" --registry "$registry" --no-audit --no-fund >/dev/null
test -x node_modules/@open-gitagent/gitagent-linux-x64/bin/gitagent
AGENT="$agent" SCRIPT="$work/script.yaml" OUT="$work/out-sdk" node --input-type=module -e '
import { query, tool, loadAgent, CostTracker, createSandboxContext, findBinary } from "@open-gitagent/gitagent";
for (const f of [query, tool, loadAgent, createSandboxContext]) if (typeof f !== "function") throw new Error("missing export");
if (typeof CostTracker !== "function") throw new Error("missing CostTracker");
if (!findBinary()?.includes("gitagent-linux-x64")) throw new Error(`binary not from the platform package: ${findBinary()}`);
let removed = false;
try { loadAgent(); } catch (e) { removed = /not part of gitagent 3/.test(e.message); }
if (!removed) throw new Error("loadAgent should explain that it is gone");
const q = query({ prompt: "say hello", dir: process.env.AGENT, model: `scripted:${process.env.SCRIPT}`, outDir: process.env.OUT });
let text = "";
for await (const m of q) if (m.type === "assistant" && m.content) text += m.content;
if (!text.includes("hello from the npm binary")) throw new Error(`unexpected reply: ${text}`);
console.log("SDK reply:", text);
'
echo "npm install checks passed"
