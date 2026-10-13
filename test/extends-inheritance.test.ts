// `extends` inheritance of tools/ and skills/.
//
// The spec (opengap SPECIFICATION.md, "extends" resolution rules) says
// `skills/` and `tools/` are a union with the child shadowing the parent on a
// name collision. Before this, only agent.yaml and RULES.md were inherited.
//
// Follows the repo convention: import compiled output from ../dist (run
// `npm run build` first).

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, existsSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";

let loadAgent: typeof import("../dist/loader.js").loadAgent;
let loadAgentDeclarativeTools: typeof import("../dist/tool-loader.js").loadAgentDeclarativeTools;
let refreshSkills: typeof import("../dist/skills.js").refreshSkills;

const MODEL = "anthropic:claude-sonnet-4-5-20250929";

before(async () => {
	({ loadAgent } = await import("../dist/loader.js"));
	({ loadAgentDeclarativeTools } = await import("../dist/tool-loader.js"));
	({ refreshSkills } = await import("../dist/skills.js"));
});

function write(path: string, text: string) {
	mkdirSync(dirname(path), { recursive: true });
	writeFileSync(path, text);
}

function manifest(name: string, extra = "") {
	return `spec_version: "0.1.0"\nname: ${name}\nversion: 0.1.0\ndescription: ${name}\nmodel:\n  preferred: ""\n  fallback: []\ntools: []\nruntime:\n  max_turns: 1\n${extra}`;
}

function tool(dir: string, name: string, description: string, script: string) {
	write(join(dir, "tools", `${name}.yaml`), `name: ${name}\ndescription: ${description}\ninput_schema:\n  type: object\n  properties: {}\nimplementation:\n  script: ${name}.sh\n`);
	write(join(dir, "tools", `${name}.sh`), script);
}

function skill(dir: string, name: string, description: string) {
	write(join(dir, "skills", name, "SKILL.md"), `---\nname: ${name}\ndescription: ${description}\n---\n\nInstructions.\n`);
}

// A parent with a tool and a skill of its own, plus one of each that the
// child overrides; a child that extends it.
function fixture() {
	const root = mkdtempSync(join(tmpdir(), "gitagent-extends-"));

	const parent = join(root, "parent");
	write(join(parent, "agent.yaml"), manifest("parent"));
	tool(parent, "parent_tool", "defined only in the parent", "pwd\n");
	tool(parent, "shared_tool", "parent version", "echo parent\n");
	skill(parent, "parent-skill", "defined only in the parent");
	skill(parent, "shared-skill", "parent version");
	execFileSync("git", ["init", "-q", "-b", "main"], { cwd: parent });
	execFileSync("git", ["add", "-A"], { cwd: parent });
	execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "parent"], { cwd: parent });

	const child = join(root, "child");
	write(join(child, "agent.yaml"), manifest("child", `extends: ${parent}\n`));
	tool(child, "shared_tool", "child version", "echo child\n");
	skill(child, "shared-skill", "child version");

	return { child };
}

describe("extends inherits the parent's skills", () => {
	it("adds parent skills, keeps the child's on a name collision, and points at their real location", async () => {
		const { child } = fixture();
		const loaded = await loadAgent(child, MODEL);

		assert.ok(loaded.parentDir, "the parent should be resolved");
		const byName = Object.fromEntries(loaded.skills.map((s) => [s.name, s]));
		assert.deepEqual(Object.keys(byName).sort(), ["parent-skill", "shared-skill"]);
		assert.equal(byName["shared-skill"].description, "child version");
		assert.equal(byName["shared-skill"].location, undefined);

		const location = byName["parent-skill"].location!;
		assert.equal(location, ".gitagent/deps/parent/skills/parent-skill/SKILL.md");
		assert.ok(existsSync(join(child, location)), "the advertised location must be readable from the agent dir");
		assert.ok(loaded.systemPrompt.includes(`<location>${location}</location>`));
		assert.ok(loaded.systemPrompt.includes("<location>skills/shared-skill/SKILL.md</location>"));

		const refreshed = await refreshSkills(child, loaded.parentDir);
		assert.deepEqual(refreshed.map((s) => s.name), ["parent-skill", "shared-skill"]);
	});
});

describe("extends inherits the parent's declarative tools", () => {
	it("adds parent tools, keeps the child's on a name collision, and runs them in the child's directory", async () => {
		const { child } = fixture();
		const loaded = await loadAgent(child, MODEL);
		const tools = await loadAgentDeclarativeTools(loaded.agentDir, loaded.parentDir);

		const byName = Object.fromEntries(tools.map((t) => [t.name, t]));
		assert.deepEqual(tools.map((t) => t.name).sort(), ["parent_tool", "shared_tool"]);
		assert.equal(byName["shared_tool"].description, "child version");

		const ran = await byName["parent_tool"].execute("call-1", {});
		assert.equal(realpathSync((ran.content[0] as { text: string }).text), realpathSync(child));
	});
});

describe("agents without extends are unchanged", () => {
	it("loads only their own skills and tools", async () => {
		const root = mkdtempSync(join(tmpdir(), "gitagent-noextends-"));
		write(join(root, "agent.yaml"), manifest("solo"));
		tool(root, "solo_tool", "solo", "echo solo\n");
		skill(root, "solo-skill", "solo");

		const loaded = await loadAgent(root, MODEL);
		assert.equal(loaded.parentDir, undefined);
		assert.deepEqual(loaded.skills.map((s) => s.name), ["solo-skill"]);
		assert.ok(loaded.systemPrompt.includes("<location>skills/solo-skill/SKILL.md</location>"));

		const tools = await loadAgentDeclarativeTools(loaded.agentDir, loaded.parentDir);
		assert.deepEqual(tools.map((t) => t.name), ["solo_tool"]);
	});
});
