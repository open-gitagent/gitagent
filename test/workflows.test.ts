// Unit tests for loadFlowDefinition in src/workflows.ts — path containment,
// structural guards, and dependency handling.

import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { loadFlowDefinition } from "../src/workflows.ts";

async function withAgentDir(files: Record<string, string>, fn: (dir: string) => Promise<void>): Promise<void> {
	const dir = await mkdtemp(join(tmpdir(), "gitagent-flows-"));
	try {
		await mkdir(join(dir, "workflows"), { recursive: true });
		for (const [name, content] of Object.entries(files)) {
			await writeFile(join(dir, "workflows", name), content, "utf-8");
		}
		await fn(dir);
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
}

const VALID_FLOW = `name: morning-digest
description: Summarize unread emails and post to Slack.
steps:
  - id: fetch
    skill: gmail
    prompt: Fetch unread emails.
  - skill: slack
    prompt: Post the digest.
    channel: "#daily-digest"
    depends_on: [fetch]
    requires_approval: true
`;

test("loadFlowDefinition loads a flow by name and preserves id/depends_on/requires_approval", async () => {
	await withAgentDir({ "morning-digest.yaml": VALID_FLOW }, async (dir) => {
		const flow = await loadFlowDefinition(dir, "morning-digest");
		assert.equal(flow.name, "morning-digest");
		assert.equal(flow.steps.length, 2);
		assert.equal(flow.steps[0].id, "fetch");
		assert.deepEqual(flow.steps[1].depends_on, ["fetch"]);
		assert.equal(flow.steps[1].requires_approval, true);
		assert.equal(flow.steps[1].channel, "#daily-digest");
	});
});

test("loadFlowDefinition rejects non-kebab-case names, including traversal attempts", async () => {
	await withAgentDir({ "morning-digest.yaml": VALID_FLOW }, async (dir) => {
		for (const bad of ["../../etc/passwd", "/etc/passwd", "..", "Not_Kebab", "with space"]) {
			await assert.rejects(() => loadFlowDefinition(dir, bad), /must be kebab-case/, `accepted "${bad}"`);
		}
	});
});

test("loadFlowDefinition reports a clear error for an empty file", async () => {
	await withAgentDir({ "empty.yaml": "" }, async (dir) => {
		await assert.rejects(() => loadFlowDefinition(dir, "empty"), /must be a YAML mapping/);
	});
});

test("loadFlowDefinition reports a clear error for a scalar document", async () => {
	await withAgentDir({ "scalar.yaml": "just a string\n" }, async (dir) => {
		await assert.rejects(() => loadFlowDefinition(dir, "scalar"), /must be a YAML mapping/);
	});
});

test("loadFlowDefinition coerces a non-string description", async () => {
	const flow = `name: numeric-desc
description: 42
steps:
  - skill: gmail
    prompt: Fetch.
`;
	await withAgentDir({ "numeric-desc.yaml": flow }, async (dir) => {
		const loaded = await loadFlowDefinition(dir, "numeric-desc");
		assert.equal(typeof loaded.description, "string");
		assert.equal(loaded.description, "42");
	});
});

test("loadFlowDefinition rejects a step with an empty skill", async () => {
	const flow = `name: empty-skill
description: missing skill
steps:
  - skill: gmail
    prompt: Fetch.
  - prompt: Post it somewhere.
`;
	await withAgentDir({ "empty-skill.yaml": flow }, async (dir) => {
		await assert.rejects(() => loadFlowDefinition(dir, "empty-skill"), /step\[1\] has an empty skill/);
	});
});

test("loadFlowDefinition still accepts the legacy single full-path argument", async () => {
	await withAgentDir({ "morning-digest.yaml": VALID_FLOW }, async (dir) => {
		const legacyPath = join(dir, "workflows", "morning-digest.yaml");
		const flow = await loadFlowDefinition(legacyPath);
		assert.equal(flow.name, "morning-digest");
		assert.equal(flow.steps.length, 2);
	});
});

test("loadFlowDefinition does not read workflows/undefined.yaml when the name is omitted", async () => {
	await withAgentDir({ "morning-digest.yaml": VALID_FLOW }, async (dir) => {
		// The legacy form treats the lone argument as a file path, so an agent dir
		// passed alone must fail as a missing file rather than silently looking for
		// a flow literally named "undefined".
		await assert.rejects(() => loadFlowDefinition(dir), (err: any) => {
			assert.ok(!String(err.message).includes("undefined"), err.message);
			return true;
		});
	});
});

test("loadFlowDefinition rejects depends_on that does not name a preceding step", async () => {
	const forward = `name: forward-dep
description: depends on a later step
steps:
  - skill: gmail
    prompt: Fetch.
    depends_on: [post]
  - id: post
    skill: slack
    prompt: Post.
`;
	const dangling = `name: dangling-dep
description: depends on nothing that exists
steps:
  - skill: gmail
    prompt: Fetch.
    depends_on: [nope]
`;
	await withAgentDir({ "forward-dep.yaml": forward, "dangling-dep.yaml": dangling }, async (dir) => {
		await assert.rejects(() => loadFlowDefinition(dir, "forward-dep"), /not the id of a preceding step/);
		await assert.rejects(() => loadFlowDefinition(dir, "dangling-dep"), /not the id of a preceding step/);
	});
});
