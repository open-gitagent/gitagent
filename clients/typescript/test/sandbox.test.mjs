// The sandbox against the real binary (GITAGENT_BIN) and a scripted model,
// with a stand-in for gitmachine whose "VM" is a temporary folder.
// Run after `npm run build`: node --test test/

import { execFileSync, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test, before, after } from "node:test";
import assert from "node:assert/strict";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = join(here, "..");
const bin = process.env.GITAGENT_BIN;
const fake = join(pkg, "node_modules", "gitmachine");
let root;

const FAKE_GITMACHINE = `
import { execSync } from "node:child_process";
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
const vm = process.env.FAKE_VM;
const local = (p) => p.replaceAll("/home/user/", vm + "/");
// The log sits next to the VM folder, which exists only after start().
const log = (line) => appendFileSync(vm.replace(/\\/[^/]+$/, "") + "/vm.log", line + "\\n");
export class GitMachine {
  constructor(options) {
    log("new " + options.repository);
    this.machine = {
      readFile: async (p) => readFileSync(local(p), "utf8"),
      writeFile: async (p, c) => writeFileSync(local(p), c),
    };
  }
  async start() { mkdirSync(vm + "/app", { recursive: true }); log("start"); }
  async run(cmd, o) {
    try {
      const out = execSync(local(cmd), { cwd: local(o.cwd), encoding: "utf8", stdio: "pipe" });
      o.onStdout?.(out);
      return { exitCode: 0 };
    } catch (e) {
      o.onStderr?.(String(e.stderr ?? ""));
      return { exitCode: e.status ?? 1 };
    }
  }
  async commit(message) { log("commit " + message); }
  async stop() { log("stop"); }
}
`;

function script(name, steps) {
  const path = join(root, name);
  writeFileSync(path, steps.map((s) => `- ${s}`).join("\n") + "\n");
  return `scripted:${path}`;
}

before(() => {
  assert.ok(bin, "GITAGENT_BIN must point to a gitagent binary");
  root = mkdtempSync(join(tmpdir(), "gitagent-sandbox-"));
  process.env.HOME = root;
  process.env.FAKE_VM = join(root, "vm");
  mkdirSync(fake, { recursive: true });
  writeFileSync(join(fake, "package.json"), JSON.stringify({ name: "gitmachine", type: "module", main: "index.js" }));
  writeFileSync(join(fake, "index.js"), FAKE_GITMACHINE);
  const agent = join(root, "agent");
  mkdirSync(agent);
  writeFileSync(join(agent, "agent.yaml"), 'spec_version: "0.1.0"\nname: lyra\nversion: 1.0.0\ndescription: t\n');
  writeFileSync(join(agent, "SOUL.md"), "I am Lyra.\n");
});

after(() => {
  rmSync(fake, { recursive: true, force: true });
  rmSync(root, { recursive: true, force: true });
});

test("query with sandbox runs every tool in the VM and stops it before the end", async () => {
  const { query } = await import("../dist/index.js");
  const model = script("q.yaml", [
    'tool_calls: [{name: cli, arguments: {command: "echo hi > hi.txt && cat hi.txt"}}]',
    'tool_calls: [{name: write, arguments: {path: src/a.txt, content: "one two"}}]',
    'tool_calls: [{name: edit, arguments: {path: src/a.txt, old_string: two, new_string: three}}]',
    'tool_calls: [{name: read, arguments: {path: src/a.txt}}]',
    'tool_calls: [{name: memory, arguments: {action: save, content: "remember this", message: "note"}}]',
    'text: "done"',
  ]);
  const q = query({
    prompt: "go",
    dir: join(root, "agent"),
    model,
    binary: bin,
    sandbox: { repository: "https://example.com/org/app.git" },
    outDir: join(root, "out-q"),
  });
  const results = [];
  for await (const m of q) if (m.type === "tool_result") results.push([m.toolName, String(m.content)]);
  const vm = process.env.FAKE_VM;
  assert.deepEqual(results.map((r) => r[0]), ["cli", "write", "edit", "read", "memory"]);
  assert.match(results[0][1], /hi/);
  assert.equal(results[3][1], "one three");
  assert.equal(readFileSync(join(vm, "app/hi.txt"), "utf8"), "hi\n");
  assert.equal(readFileSync(join(vm, "app/memory/MEMORY.md"), "utf8"), "remember this");
  assert.ok(!existsSync(join(root, "agent", "hi.txt")), "nothing ran on this machine");
  const log = readFileSync(join(root, "vm.log"), "utf8").trim().split("\n");
  assert.deepEqual(log, ["new https://example.com/org/app.git", "start", "commit note", "stop"]);
});

test("gitagent -s runs a task in the sandbox from the command line", () => {
  rmSync(join(root, "vm.log"), { force: true });
  const model = script("c.yaml", ['tool_calls: [{name: cli, arguments: {command: "echo cli > c.txt"}}]', 'text: "made c.txt"']);
  const out = spawnSync(
    process.execPath,
    [join(pkg, "bin/gitagent.js"), "-s", "--sandbox-repo", "https://example.com/org/app.git", "-d", join(root, "agent"), "-m", model, "-p", "go"],
    { encoding: "utf8", env: process.env },
  );
  assert.equal(out.status, 0, out.stderr);
  assert.match(out.stdout, /made c\.txt/);
  assert.equal(readFileSync(join(process.env.FAKE_VM, "app/c.txt"), "utf8"), "cli\n");
  assert.deepEqual(readFileSync(join(root, "vm.log"), "utf8").trim().split("\n"), ["new https://example.com/org/app.git", "start", "stop"]);
});

test("without gitmachine, -s says how to install it", () => {
  const moved = fake + ".off";
  execFileSync("mv", [fake, moved]);
  try {
    const out = spawnSync(process.execPath, [join(pkg, "bin/gitagent.js"), "-s", "-p", "go"], { encoding: "utf8", env: process.env });
    assert.equal(out.status, 2);
    assert.match(out.stderr, /npm install -g gitmachine/);
  } finally {
    execFileSync("mv", [moved, fake]);
  }
});
