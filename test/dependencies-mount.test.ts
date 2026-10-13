// `dependencies[].mount`.
//
// The spec (opengap SPECIFICATION.md, "dependencies") composes external agents
// "as mounted capabilities", e.g. `mount: agents/fact-checker`, and the README
// mounts a shared-tools repo at `tools`. Before this, dependencies were cloned
// into .gitagent/deps/<name> and `mount` was never read, so nothing was
// mounted.
//
// Follows the repo convention: import compiled output from ../dist (run
// `npm run build` first).

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, existsSync, lstatSync, realpathSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";

let loadAgent: typeof import("../dist/loader.js").loadAgent;
let loadDeclarativeTools: typeof import("../dist/tool-loader.js").loadDeclarativeTools;

const MODEL = "anthropic:claude-sonnet-4-5-20250929";

before(async () => {
	({ loadAgent } = await import("../dist/loader.js"));
	({ loadDeclarativeTools } = await import("../dist/tool-loader.js"));
});

function write(path: string, text: string) {
	mkdirSync(dirname(path), { recursive: true });
	writeFileSync(path, text);
}

function manifest(name: string, description: string, extra = "") {
	return `spec_version: "0.1.0"\nname: ${name}\nversion: 0.1.0\ndescription: ${description}\nmodel:\n  preferred: ""\n  fallback: []\ntools: []\nruntime:\n  max_turns: 1\n${extra}`;
}

function commit(dir: string) {
	execFileSync("git", ["init", "-q", "-b", "main"], { cwd: dir });
	execFileSync("git", ["add", "-A"], { cwd: dir });
	execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"], { cwd: dir });
}

// An agent repo to use as a sub-agent dependency.
function helperRepo(root: string) {
	const dir = join(root, "helper");
	write(join(dir, "agent.yaml"), manifest("helper", "a mounted helper agent"));
	commit(dir);
	return dir;
}

// A repo of shared declarative tools, laid out to be mounted at `tools`.
function sharedToolsRepo(root: string) {
	const dir = join(root, "shared-tools");
	write(join(dir, "shared_tool.yaml"), "name: shared_tool\ndescription: from a dependency\ninput_schema:\n  type: object\n  properties: {}\nimplementation:\n  script: shared_tool.sh\n");
	write(join(dir, "shared_tool.sh"), "echo shared\n");
	commit(dir);
	return dir;
}

function agentWithDeps(root: string, deps: Array<{ name: string; source: string; mount?: string }>) {
	const dir = join(root, "agent");
	const yaml = deps
		.map((d) => `  - name: ${d.name}\n    source: ${d.source}\n    version: main\n${d.mount ? `    mount: ${d.mount}\n` : ""}`)
		.join("");
	write(join(dir, "agent.yaml"), manifest("agent", "agent", `dependencies:\n${yaml}`));
	return dir;
}

describe("dependencies are mounted at their mount path", () => {
	it("mounts an agent under agents/ so it is discovered as a sub-agent", async () => {
		const root = mkdtempSync(join(tmpdir(), "gitagent-mount-"));
		const agent = agentWithDeps(root, [{ name: "helper", source: helperRepo(root), mount: "agents/helper" }]);

		const loaded = await loadAgent(agent, MODEL);

		const mounted = join(agent, "agents", "helper");
		assert.ok(lstatSync(mounted).isSymbolicLink());
		assert.equal(realpathSync(mounted), realpathSync(join(agent, ".gitagent", "deps", "helper")));
		assert.deepEqual(loaded.subAgents.map((a) => a.name), ["helper"]);
		assert.ok(loaded.systemPrompt.includes("a mounted helper agent"));
	});

	it("mounts a shared-tools repo at tools, as in the README", async () => {
		const root = mkdtempSync(join(tmpdir(), "gitagent-mount-"));
		const agent = agentWithDeps(root, [{ name: "shared-tools", source: sharedToolsRepo(root), mount: "tools" }]);

		await loadAgent(agent, MODEL);
		const tools = await loadDeclarativeTools(agent);
		assert.deepEqual(tools.map((t) => t.name), ["shared_tool"]);
	});

	it("is idempotent across runs", async () => {
		const root = mkdtempSync(join(tmpdir(), "gitagent-mount-"));
		const agent = agentWithDeps(root, [{ name: "helper", source: helperRepo(root), mount: "agents/helper" }]);

		await loadAgent(agent, MODEL);
		const second = await loadAgent(agent, MODEL);
		assert.deepEqual(second.subAgents.map((a) => a.name), ["helper"]);
	});
});

describe("mount paths are confined and never clobber", () => {
	it("refuses mount paths outside the agent directory", async () => {
		const root = mkdtempSync(join(tmpdir(), "gitagent-mount-"));
		const source = helperRepo(root);
		const agent = agentWithDeps(root, [
			{ name: "up", source, mount: "../escaped" },
			{ name: "abs", source, mount: join(root, "absolute") },
			{ name: "self", source, mount: "." },
		]);

		await loadAgent(agent, MODEL);
		assert.equal(existsSync(join(root, "escaped")), false);
		assert.equal(existsSync(join(root, "absolute")), false);
		assert.equal(lstatSync(agent).isSymbolicLink(), false);
	});

	it("leaves an existing path alone", async () => {
		const root = mkdtempSync(join(tmpdir(), "gitagent-mount-"));
		const agent = agentWithDeps(root, [{ name: "helper", source: helperRepo(root), mount: "agents/helper" }]);
		write(join(agent, "agents", "helper", "notes.md"), "mine\n");

		await loadAgent(agent, MODEL);
		const mounted = join(agent, "agents", "helper");
		assert.equal(lstatSync(mounted).isSymbolicLink(), false);
		assert.deepEqual(readdirSync(mounted), ["notes.md"]);
	});
});

describe("dependencies without mount are unchanged", () => {
	it("clones into .gitagent/deps and mounts nothing", async () => {
		const root = mkdtempSync(join(tmpdir(), "gitagent-mount-"));
		const agent = agentWithDeps(root, [{ name: "helper", source: helperRepo(root) }]);

		const loaded = await loadAgent(agent, MODEL);
		assert.ok(existsSync(join(agent, ".gitagent", "deps", "helper", "agent.yaml")));
		assert.equal(existsSync(join(agent, "agents")), false);
		assert.deepEqual(loaded.subAgents, []);
	});
});
