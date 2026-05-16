// Unit tests for src/utils/workflow-generator.ts and the retry loop in
// src/commands/workflow.ts. The LLM is fully mocked — no network calls.

import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
	buildMessages,
	buildSystemPrompt,
	stripCodeFences,
	generateWorkflow,
	type LlmClient,
	type LlmMessage,
} from "../src/utils/workflow-generator.ts";
import { runGenerate } from "../src/commands/workflow.ts";
import type { SkillMetadata } from "../src/skills.ts";

const SKILLS: SkillMetadata[] = [
	{ name: "gmail", description: "Read and send email", directory: "/x/skills/gmail", filePath: "/x/skills/gmail/SKILL.md" },
	{ name: "slack", description: "Post to Slack", directory: "/x/skills/slack", filePath: "/x/skills/slack/SKILL.md" },
	{ name: "summarize", description: "Summarize text", directory: "/x/skills/summarize", filePath: "/x/skills/summarize/SKILL.md" },
];

const VALID_YAML = `name: morning-digest
description: Summarize unread emails and post to Slack each morning.
steps:
  - skill: gmail
    prompt: Fetch unread emails.
  - skill: summarize
    prompt: Compose a digest.
  - skill: slack
    prompt: Post the digest.
    channel: "#daily-digest"
`;

const INVALID_YAML_MISSING_NAME = `description: no name here
steps:
  - skill: gmail
    prompt: hi
`;

// ── buildSystemPrompt / buildMessages ──────────────────────────────────

test("buildSystemPrompt embeds the schema text and the skill list", () => {
	const sys = buildSystemPrompt(SKILLS);
	assert.ok(sys.includes("<schema>"), "system prompt missing <schema> tag");
	assert.ok(sys.includes('"$id": "https://gitclaw.dev/spec/workflow.schema.json"'), "system prompt missing schema $id");
	assert.ok(sys.includes("- gmail: Read and send email"), "system prompt missing gmail skill");
	assert.ok(sys.includes("- slack: Post to Slack"), "system prompt missing slack skill");
	assert.ok(sys.includes("Output ONLY valid YAML"), "system prompt missing rule about raw YAML");
});

test("buildSystemPrompt handles empty skill list with a fallback hint", () => {
	const sys = buildSystemPrompt([]);
	assert.ok(sys.includes("no installed skills detected"), "system prompt missing empty-skills fallback");
});

test("buildMessages includes two few-shot pairs and the user prompt", () => {
	const messages = buildMessages({ prompt: "Do the thing", skills: SKILLS });
	assert.equal(messages[0].role, "system");
	assert.equal(messages[1].role, "user");
	assert.equal(messages[2].role, "assistant");
	assert.equal(messages[3].role, "user");
	assert.equal(messages[4].role, "assistant");
	assert.equal(messages[5].role, "user");
	assert.equal(messages[5].content, "Do the thing");
});

test("buildMessages wraps refine-mode prompts with the previous YAML and instruction", () => {
	const messages = buildMessages({
		prompt: "Add an approval step before the Slack post.",
		skills: SKILLS,
		previousWorkflow: VALID_YAML,
	});
	const last = messages[messages.length - 1];
	assert.equal(last.role, "user");
	assert.ok(last.content.includes("Here is the current workflow"));
	assert.ok(last.content.includes("Add an approval step before the Slack post."));
	assert.ok(last.content.includes("morning-digest"));
	assert.ok(last.content.includes("Return the complete updated workflow as YAML — not a diff."));
});

// ── stripCodeFences ────────────────────────────────────────────────────

test("stripCodeFences removes generic fenced code", () => {
	const input = "```\nname: foo\n```\n";
	assert.equal(stripCodeFences(input), "name: foo");
});

test("stripCodeFences removes yaml-tagged fences", () => {
	const input = "```yaml\nname: foo\n```";
	assert.equal(stripCodeFences(input), "name: foo");
});

test("stripCodeFences leaves unfenced YAML alone", () => {
	const input = "name: foo\n";
	assert.equal(stripCodeFences(input), "name: foo");
});

// ── generateWorkflow with an injected LLM ──────────────────────────────

test("generateWorkflow returns the LLM output after stripping fences", async () => {
	const captured: { messages: LlmMessage[] } = { messages: [] };
	const llm: LlmClient = async (messages) => {
		captured.messages = messages;
		return "```yaml\n" + VALID_YAML + "```";
	};
	const out = await generateWorkflow({
		prompt: "summarize emails and post to slack",
		skills: SKILLS,
		llm,
	});
	assert.equal(out.trim().startsWith("name: morning-digest"), true);
	assert.equal(captured.messages.length, 6);
	assert.equal(captured.messages[0].role, "system");
});

test("generateWorkflow throws if prompt is empty", async () => {
	await assert.rejects(
		() => generateWorkflow({ prompt: "   ", skills: SKILLS, llm: async () => VALID_YAML }),
		/prompt is required/,
	);
});

// ── Retry loop in runGenerate ──────────────────────────────────────────

test("runGenerate retries when validation fails, then writes the file when the second attempt is valid", async () => {
	const dir = await mkdtemp(join(tmpdir(), "gitclaw-test-"));
	try {
		let calls = 0;
		const llm: LlmClient = async (messages) => {
			calls++;
			const userMsg = messages[messages.length - 1].content;
			if (calls === 1) return INVALID_YAML_MISSING_NAME;
			// Second attempt: ensure the retry prompt included the validation error.
			assert.ok(userMsg.includes("schema validation"), `retry user message did not mention validation: ${userMsg}`);
			return VALID_YAML;
		};
		const result = await runGenerate({
			flags: {
				dir,
				prompt: "summarize unread emails and post to Slack",
				dryRun: false,
			},
			llm,
		});
		assert.equal(calls, 2);
		assert.ok(result.filePath, "expected a written file path");
		assert.equal(result.filePath!.endsWith("workflows/morning-digest.yaml"), true);
		const written = await readFile(result.filePath!, "utf-8");
		assert.ok(written.includes("name: morning-digest"));
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test("runGenerate honours --dry-run by returning YAML without writing", async () => {
	const dir = await mkdtemp(join(tmpdir(), "gitclaw-test-"));
	try {
		const llm: LlmClient = async () => VALID_YAML;
		const result = await runGenerate({
			flags: { dir, prompt: "x", dryRun: true },
			llm,
		});
		assert.equal(result.filePath, undefined);
		assert.ok(result.yaml.includes("name: morning-digest"));
		// workflows/ must not have been created.
		await assert.rejects(() => readFile(join(dir, "workflows", "morning-digest.yaml"), "utf-8"));
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test("runGenerate gives up after MAX_RETRIES and throws", async () => {
	const dir = await mkdtemp(join(tmpdir(), "gitclaw-test-"));
	try {
		let calls = 0;
		const llm: LlmClient = async () => {
			calls++;
			return INVALID_YAML_MISSING_NAME;
		};
		await assert.rejects(
			() => runGenerate({ flags: { dir, prompt: "x", dryRun: true }, llm }),
			/Validation failed after retries/,
		);
		assert.equal(calls, 3); // 1 initial + 2 retries
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});

test("runGenerate refine mode reads previous YAML and passes it to the LLM", async () => {
	const dir = await mkdtemp(join(tmpdir(), "gitclaw-test-"));
	try {
		const { writeFile, mkdir } = await import("node:fs/promises");
		await mkdir(join(dir, "workflows"), { recursive: true });
		const refinePath = join(dir, "workflows", "starter.yaml");
		await writeFile(refinePath, VALID_YAML, "utf-8");

		let observed = "";
		const llm: LlmClient = async (messages) => {
			observed = messages[messages.length - 1].content;
			return VALID_YAML;
		};
		await runGenerate({
			flags: {
				dir,
				prompt: "add an approval step before slack",
				refine: "workflows/starter.yaml",
				dryRun: true,
			},
			llm,
		});
		assert.ok(observed.includes("Here is the current workflow"));
		assert.ok(observed.includes("add an approval step before slack"));
	} finally {
		await rm(dir, { recursive: true, force: true });
	}
});
