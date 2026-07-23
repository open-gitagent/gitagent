import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

import { loadAgent } from "../dist/loader.js";

const ENV_NAMES = [
	"OPENAI_API_KEY",
	"ATLASCLOUD_API_KEY",
	"ATLAS_CLOUD_API_KEY",
	"ATLASCLOUD_API_BASE",
	"ATLASCLOUD_BASE_URL",
	"ATLAS_CLOUD_API_BASE",
	"ATLAS_CLOUD_BASE_URL",
	"GITAGENT_MODEL_BASE_URL",
];

async function withEnv(
	values: Record<string, string | undefined>,
	fn: () => Promise<void>,
): Promise<void> {
	const previous = new Map(ENV_NAMES.map((name) => [name, process.env[name]]));
	for (const name of ENV_NAMES) {
		delete process.env[name];
	}
	for (const [name, value] of Object.entries(values)) {
		if (value === undefined) {
			delete process.env[name];
		} else {
			process.env[name] = value;
		}
	}

	try {
		await fn();
	} finally {
		for (const name of ENV_NAMES) {
			const value = previous.get(name);
			if (value === undefined) {
				delete process.env[name];
			} else {
				process.env[name] = value;
			}
		}
	}
}

async function makeAgentDir(model: string): Promise<string> {
	const dir = await mkdtemp(join(tmpdir(), "gitagent-atlascloud-"));
	await writeFile(
		join(dir, "agent.yaml"),
		[
			'spec_version: "0.1.0"',
			"name: atlascloud-test",
			"version: 0.1.0",
			"description: Test agent",
			"model:",
			`  preferred: "${model}"`,
			"  fallback: []",
			"tools: []",
			"runtime:",
			"  max_turns: 1",
			"",
		].join("\n"),
		"utf-8",
	);
	return dir;
}

test("atlascloud model shortcut creates an OpenAI-compatible Atlas Cloud model", async () => {
	await withEnv({ ATLASCLOUD_API_KEY: "atlas-key" }, async () => {
		const dir = await makeAgentDir("atlascloud:qwen/qwen3.5-flash");
		const loaded = await loadAgent(dir);

		assert.equal(loaded.model.id, "qwen/qwen3.5-flash");
		assert.equal(loaded.model.baseUrl, "https://api.atlascloud.ai/v1");
		assert.equal(loaded.model.api, "openai-completions");
		assert.equal(loaded.model.provider, "openai");
		assert.equal(process.env.OPENAI_API_KEY, "atlas-key");
	});
});

test("atlas-cloud alias supports key and base URL environment aliases", async () => {
	await withEnv(
		{
			ATLAS_CLOUD_API_KEY: "alias-key",
			ATLASCLOUD_BASE_URL: "https://atlas.example.test/v1",
		},
		async () => {
			const dir = await makeAgentDir("atlas-cloud:deepseek-ai/deepseek-v4-pro");
			const loaded = await loadAgent(dir);

			assert.equal(loaded.model.id, "deepseek-ai/deepseek-v4-pro");
			assert.equal(loaded.model.baseUrl, "https://atlas.example.test/v1");
			assert.equal(loaded.model.provider, "openai");
			assert.equal(process.env.OPENAI_API_KEY, "alias-key");
		},
	);
});

test("explicit atlas endpoint keeps the inline base URL and uses Atlas key aliases", async () => {
	await withEnv({ ATLAS_CLOUD_API_KEY: "inline-key" }, async () => {
		const dir = await makeAgentDir("atlas:qwen/qwen3.5-flash@https://proxy.example.test/v1");
		const loaded = await loadAgent(dir);

		assert.equal(loaded.model.id, "qwen/qwen3.5-flash");
		assert.equal(loaded.model.baseUrl, "https://proxy.example.test/v1");
		assert.equal(loaded.model.provider, "openai");
		assert.equal(process.env.OPENAI_API_KEY, "inline-key");
	});
});
