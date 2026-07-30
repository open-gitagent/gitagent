import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

let knowledge: typeof import("../dist/knowledge.js");

before(async () => {
	knowledge = await import("../dist/knowledge.js");
});

// Build a temp agent dir with a knowledge/ index + docs.
async function makeAgent(indexYaml: string, docs: Record<string, string> = {}): Promise<string> {
	const dir = await mkdtemp(join(tmpdir(), "gitagent-kn-"));
	const kdir = join(dir, "knowledge");
	await mkdir(kdir, { recursive: true });
	await writeFile(join(kdir, "index.yaml"), indexYaml, "utf-8");
	for (const [name, body] of Object.entries(docs)) {
		await writeFile(join(kdir, name), body, "utf-8");
	}
	return dir;
}

describe("loadKnowledge", () => {
	it("returns empty when there is no knowledge index", async () => {
		const dir = await mkdtemp(join(tmpdir(), "gitagent-kn-empty-"));
		const k = await knowledge.loadKnowledge(dir);
		assert.deepEqual(k, { preloaded: [], available: [] });
		await rm(dir, { recursive: true, force: true });
	});

	it("preloads always_load docs and lists the rest as available", async () => {
		const dir = await makeAgent(
			[
				"entries:",
				"  - path: policy.md",
				"    tags: [hr]",
				"    priority: high",
				"    always_load: true",
				"  - path: faq.md",
				"    tags: [support]",
				"    priority: low",
				"",
			].join("\n"),
			{ "policy.md": "  Leave policy: 20 days.  ", "faq.md": "Q&A" },
		);

		const k = await knowledge.loadKnowledge(dir);
		assert.equal(k.preloaded.length, 1);
		assert.equal(k.preloaded[0].path, "policy.md");
		assert.equal(k.preloaded[0].content, "Leave policy: 20 days.", "content is trimmed");
		assert.equal(k.available.length, 1);
		assert.equal(k.available[0].path, "faq.md");
		await rm(dir, { recursive: true, force: true });
	});

	it("skips always_load entries whose file is missing", async () => {
		const dir = await makeAgent(
			[
				"entries:",
				"  - path: gone.md",
				"    tags: []",
				"    priority: high",
				"    always_load: true",
				"",
			].join("\n"),
			// no gone.md written
		);
		const k = await knowledge.loadKnowledge(dir);
		assert.deepEqual(k.preloaded, []);
		assert.deepEqual(k.available, []);
		await rm(dir, { recursive: true, force: true });
	});

	it("returns empty for a malformed index", async () => {
		const dir = await makeAgent("not: [a, valid, entries, list]\n");
		const k = await knowledge.loadKnowledge(dir);
		assert.deepEqual(k, { preloaded: [], available: [] });
		await rm(dir, { recursive: true, force: true });
	});
});

describe("formatKnowledgeForPrompt", () => {
	it("returns empty string when there is nothing to inject", () => {
		assert.equal(knowledge.formatKnowledgeForPrompt({ preloaded: [], available: [] }), "");
	});

	it("inlines preloaded docs and lists available ones with a read hint", () => {
		const out = knowledge.formatKnowledgeForPrompt({
			preloaded: [{ path: "policy.md", content: "Leave: 20 days" }],
			available: [{ path: "faq.md", tags: ["support"], priority: "low" }],
		});
		assert.match(out, /^# Knowledge/);
		assert.match(out, /<knowledge path="policy.md">\nLeave: 20 days\n<\/knowledge>/);
		assert.match(out, /<doc path="knowledge\/faq.md" priority="low" tags="support" \/>/);
		assert.match(out, /Use the `read` tool/);
	});
});
