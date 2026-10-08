// Regression tests for the bug/security fix batch.
//
// Follows the repo convention: import compiled output from ../dist (`npm test`
// builds first via the pretest script).

import { describe, it, before, after, mock } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, existsSync, rmSync, mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { registerFauxProvider, fauxAssistantMessage, fauxText, fauxToolCall } from "@mariozechner/pi-ai";

let createMemoryTool: typeof import("../dist/tools/memory.js").createMemoryTool;
let createSkillLearnerTool: typeof import("../dist/tools/skill-learner.js").createSkillLearnerTool;
let createEditTool: typeof import("../dist/tools/edit.js").createEditTool;
let paginateLines: typeof import("../dist/tools/shared.js").paginateLines;
let resolveSandboxPath: typeof import("../dist/tools/shared.js").resolveSandboxPath;
let MAX_BYTES: number;
let deleteSchedule: typeof import("../dist/schedules.js").deleteSchedule;
let deleteFlowDefinition: typeof import("../dist/workflows.js").deleteFlowDefinition;
let safeChildPath: typeof import("../dist/safe-path.js").safeChildPath;
let runHooks: typeof import("../dist/hooks.js").runHooks;
let wrapToolWithHooks: typeof import("../dist/hooks.js").wrapToolWithHooks;
let initLocalSession: typeof import("../dist/session.js").initLocalSession;
let redactToken: typeof import("../dist/session.js").redactToken;
let query: typeof import("../dist/exports.js").query;
let loadAgent: typeof import("../dist/loader.js").loadAgent;
let syncGitRepo: typeof import("../dist/loader.js").syncGitRepo;
let scheduleAt: typeof import("../dist/schedule-runner.js").scheduleAt;
let MAX_TIMEOUT_MS: number;
let applyExtraSampling: typeof import("../dist/model-options.js").applyExtraSampling;

const GIT_ENV = {
	GIT_AUTHOR_NAME: "test",
	GIT_AUTHOR_EMAIL: "test@example.com",
	GIT_COMMITTER_NAME: "test",
	GIT_COMMITTER_EMAIL: "test@example.com",
};

before(async () => {
	Object.assign(process.env, GIT_ENV);
	({ createMemoryTool } = await import("../dist/tools/memory.js"));
	({ createSkillLearnerTool } = await import("../dist/tools/skill-learner.js"));
	({ createEditTool } = await import("../dist/tools/edit.js"));
	({ paginateLines, resolveSandboxPath, MAX_BYTES } = await import("../dist/tools/shared.js"));
	({ deleteSchedule } = await import("../dist/schedules.js"));
	({ deleteFlowDefinition } = await import("../dist/workflows.js"));
	({ safeChildPath } = await import("../dist/safe-path.js"));
	({ runHooks, wrapToolWithHooks } = await import("../dist/hooks.js"));
	({ initLocalSession, redactToken } = await import("../dist/session.js"));
	({ query } = await import("../dist/exports.js"));
	({ loadAgent, syncGitRepo } = await import("../dist/loader.js"));
	({ scheduleAt, MAX_TIMEOUT_MS } = await import("../dist/schedule-runner.js"));
	({ applyExtraSampling } = await import("../dist/model-options.js"));
});

function git(cwd: string, ...args: string[]): string {
	return execFileSync("git", args, { cwd, stdio: "pipe", encoding: "utf-8" }).trim();
}

function tmpRepo(prefix: string): string {
	const dir = mkdtempSync(join(tmpdir(), prefix));
	git(dir, "init", "-q");
	git(dir, "commit", "-q", "--allow-empty", "-m", "init");
	return dir;
}

function sentinelPath(): string {
	return join(tmpdir(), `gitagent-sentinel-${process.pid}-${Math.random().toString(36).slice(2)}`);
}

// ── 1: git commit messages are passed as argv ───────────────────────────

describe("memory tool commit message", () => {
	it("commits the message literally without shell evaluation", async () => {
		const dir = tmpRepo("gitagent-mem-");
		const sentinel = sentinelPath();
		const msg = `note $(touch ${sentinel}) \`touch ${sentinel}\` "quoted"`;
		const memTool = createMemoryTool(dir);
		await memTool.execute("t1", { action: "save", content: "remember this", message: msg } as any);
		assert.equal(existsSync(sentinel), false, "shell substitution must not run");
		assert.equal(git(dir, "log", "-1", "--format=%B"), msg);
		rmSync(dir, { recursive: true, force: true });
	});
});

// ── 2: skill_learner name validation and scoped commits ─────────────────

describe("skill_learner update/delete", () => {
	it("rejects names that are not kebab-case", async () => {
		const dir = tmpRepo("gitagent-skill-");
		const outside = join(dir, "outside");
		mkdirSync(outside);
		writeFileSync(join(outside, "keep.txt"), "x");
		const sl = createSkillLearnerTool(dir, join(dir, ".gitagent"));
		for (const name of ["../outside", "..", "a/b", "Bad Name"]) {
			await assert.rejects(() => sl.execute("t", { action: "delete", skill_name: name } as any), /kebab-case/);
			await assert.rejects(
				() => sl.execute("t", { action: "update", skill_name: name, instructions: "x" } as any),
				/kebab-case/,
			);
		}
		assert.equal(existsSync(join(outside, "keep.txt")), true);
		rmSync(dir, { recursive: true, force: true });
	});

	it("delete commits only the skill directory", async () => {
		const dir = tmpRepo("gitagent-skill-");
		mkdirSync(join(dir, "skills", "my-skill"), { recursive: true });
		writeFileSync(join(dir, "skills", "my-skill", "SKILL.md"), "---\nname: my-skill\ndescription: d\n---\nbody\n");
		git(dir, "add", "-A");
		git(dir, "commit", "-q", "-m", "add skill");
		// Unrelated user work, staged and unstaged
		writeFileSync(join(dir, "staged.txt"), "s");
		git(dir, "add", "staged.txt");
		writeFileSync(join(dir, "untracked.txt"), "u");

		const sl = createSkillLearnerTool(dir, join(dir, ".gitagent"));
		await sl.execute("t", { action: "delete", skill_name: "my-skill" } as any);

		assert.equal(existsSync(join(dir, "skills", "my-skill")), false);
		const files = git(dir, "show", "--name-only", "--format=", "HEAD").split("\n").filter(Boolean);
		assert.deepEqual(files, ["skills/my-skill/SKILL.md"]);
		const status = git(dir, "status", "--porcelain");
		assert.match(status, /^A  staged\.txt$/m, "unrelated staged file must stay uncommitted");
		assert.match(status, /^\?\? untracked\.txt$/m);
		rmSync(dir, { recursive: true, force: true });
	});
});

// ── 3: session mode keeps the token out of URLs and errors ──────────────

describe("session token handling", () => {
	it("redactToken strips raw and encoded token", () => {
		const token = "ghp_secretvalue123";
		const b64 = Buffer.from(`x-access-token:${token}`).toString("base64");
		const out = redactToken(`fail ${token} hdr ${b64} https://${token}@github.com/x`, token);
		assert.equal(out.includes(token), false);
		assert.equal(out.includes(b64), false);
	});

	it("clone failure error does not contain the token", () => {
		const token = "ghp_shouldnotleak987";
		const parent = mkdtempSync(join(tmpdir(), "gitagent-sess-"));
		let message = "";
		try {
			initLocalSession({ url: "https://127.0.0.1:1/org/repo.git", token, dir: join(parent, "repo") });
		} catch (err: any) {
			message = String(err.message) + String(err.stack ?? "");
		}
		assert.ok(message.length > 0, "clone should fail");
		assert.equal(message.includes(token), false);
		rmSync(parent, { recursive: true, force: true });
	});

	it("rejects session branch names that look like options", () => {
		assert.throws(
			() => initLocalSession({ url: "https://127.0.0.1:1/x.git", token: "t", dir: join(tmpdir(), "nope"), session: "--upload-pack=x" }),
			/Invalid session branch/,
		);
	});
});

// ── 4: delete helpers stay inside their directory ───────────────────────

describe("path-confined deletes", () => {
	it("deleteSchedule / deleteFlowDefinition reject traversal", async () => {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-del-"));
		mkdirSync(join(dir, "schedules"));
		mkdirSync(join(dir, "workflows"));
		writeFileSync(join(dir, "agent.yaml"), "name: x\n");
		writeFileSync(join(dir, "secret.yaml"), "x");
		await assert.rejects(() => deleteSchedule(dir, "../secret"), /Invalid schedule id/);
		await assert.rejects(() => deleteFlowDefinition(dir, "../agent"), /Invalid flow name/);
		await assert.rejects(() => deleteSchedule(dir, "/etc/passwd"), /Invalid/);
		assert.equal(existsSync(join(dir, "secret.yaml")), true);
		assert.equal(existsSync(join(dir, "agent.yaml")), true);

		writeFileSync(join(dir, "schedules", "daily.yaml"), "id: daily\n");
		await deleteSchedule(dir, "daily");
		assert.equal(existsSync(join(dir, "schedules", "daily.yaml")), false);
		rmSync(dir, { recursive: true, force: true });
	});

	it("safeChildPath only returns direct children", () => {
		assert.throws(() => safeChildPath("/base", ".."));
		assert.throws(() => safeChildPath("/base", "."));
		assert.throws(() => safeChildPath("/base", "a/b"));
		assert.throws(() => safeChildPath("/base", ""));
		assert.equal(safeChildPath("/base", "my-plugin"), "/base/my-plugin");
	});
});

// ── 5: pre_tool_use hooks fail closed ───────────────────────────────────

describe("pre_tool_use fail-closed", () => {
	it("blocks the tool when a guard hook exits non-zero", async () => {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-hook-"));
		mkdirSync(join(dir, "hooks"));
		writeFileSync(join(dir, "hooks", "crash.sh"), "exit 3\n");
		let ran = false;
		const t: any = { name: "probe", execute: async () => { ran = true; return { content: [], details: undefined }; } };
		const wrapped = wrapToolWithHooks(t, { hooks: { pre_tool_use: [{ script: "crash.sh" }] } }, dir, "s");
		await assert.rejects(() => wrapped.execute("id", {}), /blocked by hook/);
		assert.equal(ran, false);

		// Non-guard hooks still fail open
		const res = await runHooks([{ script: "crash.sh" }], dir, { event: "post_response" });
		assert.equal(res.action, "allow");
		rmSync(dir, { recursive: true, force: true });
	});

	it("lets the tool run only when the failing hook sets fail_open: true", async () => {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-hook-"));
		mkdirSync(join(dir, "hooks"));
		writeFileSync(join(dir, "hooks", "crash.sh"), "exit 3\n");
		let ran = false;
		const t: any = { name: "probe", execute: async () => { ran = true; return { content: [], details: undefined }; } };

		const explicitClosed = wrapToolWithHooks(t, { hooks: { pre_tool_use: [{ script: "crash.sh", fail_open: false }] } }, dir, "s");
		await assert.rejects(() => explicitClosed.execute("id", {}), /blocked by hook/);
		assert.equal(ran, false);

		const advisory = wrapToolWithHooks(t, { hooks: { pre_tool_use: [{ script: "crash.sh", fail_open: true }] } }, dir, "s");
		await advisory.execute("id", {});
		assert.equal(ran, true);
		rmSync(dir, { recursive: true, force: true });
	});
});

// ── 8: read pagination by byte budget ───────────────────────────────────

describe("paginateLines", () => {
	it("pages through a large file without skipping or repeating lines", () => {
		const line = "é".repeat(1000); // 2000 bytes per line
		const lines = Array.from({ length: 120 }, (_, i) => `${i}:${line}`);
		const text = lines.join("\n");
		const seen: string[] = [];
		let offset = 1;
		for (let guard = 0; guard < 50; guard++) {
			const page = paginateLines(text, offset);
			assert.ok(Buffer.byteLength(page.text, "utf-8") <= MAX_BYTES);
			assert.equal(page.shownRange[0], offset);
			seen.push(...page.text.split("\n"));
			if (!page.hasMore) break;
			offset = page.shownRange[1] + 1;
		}
		assert.deepEqual(seen, lines);
	});

	it("handles a single line larger than the budget", () => {
		const text = ["€".repeat(MAX_BYTES), "next"].join("\n");
		const page = paginateLines(text, 1);
		assert.deepEqual(page.shownRange, [1, 1]);
		assert.equal(page.hasMore, true);
		assert.equal(page.text.includes("�"), false, "must not split a multi-byte char");
		assert.equal(paginateLines(text, 2).text, "next");
	});
});

// ── 9: edit tool inserts new_string literally ───────────────────────────

describe("edit tool replacement", () => {
	it("does not expand $ patterns in new_string", async () => {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-edit-"));
		writeFileSync(join(dir, "f.txt"), "price: X\n");
		const edit = createEditTool(dir);
		await edit.execute("t", { path: "f.txt", old_string: "X", new_string: "$& $$ $` $'" } as any);
		assert.equal(readFileSync(join(dir, "f.txt"), "utf-8"), "price: $& $$ $` $'\n");
		rmSync(dir, { recursive: true, force: true });
	});
});

// ── 6 / 7: SDK query multi-turn, sampling options, max_turns ────────────

describe("query() with a faux model", () => {
	let faux: ReturnType<typeof registerFauxProvider>;
	let agentDir: string;

	before(() => {
		// Route the openai-completions API (used for provider:model@url) to faux.
		faux = registerFauxProvider({ api: "openai-completions" });
		agentDir = mkdtempSync(join(tmpdir(), "gitagent-faux-"));
		writeFileSync(join(agentDir, "agent.yaml"), [
			'spec_version: "0.1.0"',
			"name: faux-agent",
			"version: 0.1.0",
			"description: test",
			"model:",
			'  preferred: "fauxprov:faux@http://127.0.0.1:9"',
			"  constraints:",
			"    temperature: 0.25",
			"    max_tokens: 123",
			"tools: []",
			"runtime:",
			"  max_turns: 50",
			"",
		].join("\n"));
		process.env.OPENAI_API_KEY ??= "test-key";
	});

	after(() => {
		faux.unregister();
		rmSync(agentDir, { recursive: true, force: true });
	});

	it("delivers every turn of a multi-turn prompt and forwards sampling options", async () => {
		const seenOptions: any[] = [];
		faux.setResponses([
			(_ctx, opts) => { seenOptions.push(opts); return fauxAssistantMessage("first"); },
			(_ctx, opts) => { seenOptions.push(opts); return fauxAssistantMessage("second"); },
		]);
		async function* prompts() {
			yield { type: "user" as const, content: "one" };
			yield { type: "user" as const, content: "two" };
		}
		const assistant: string[] = [];
		for await (const msg of query({ prompt: prompts(), dir: agentDir, replaceBuiltinTools: true })) {
			if (msg.type === "assistant") assistant.push(msg.content);
			if (msg.type === "system" && msg.subtype === "error") assert.fail(msg.content);
		}
		assert.deepEqual(assistant, ["first", "second"]);
		assert.equal(seenOptions[0]?.temperature, 0.25);
		assert.equal(seenOptions[0]?.maxTokens, 123);
	});

	it("stops the loop at maxTurns", async () => {
		// Count model requests that were actually allowed to proceed (an
		// already-aborted signal means no request would be sent).
		let liveCalls = 0;
		const loop = (_ctx: any, opts: any) => {
			if (!opts?.signal?.aborted) liveCalls++;
			return fauxAssistantMessage([fauxToolCall("ping", {})], { stopReason: "toolUse" });
		};
		faux.setResponses([loop, loop, loop, loop, loop, () => fauxAssistantMessage(fauxText("done"))]);
		const msgs: any[] = [];
		for await (const msg of query({
			prompt: "go",
			dir: agentDir,
			replaceBuiltinTools: true,
			maxTurns: 2,
			tools: [{ name: "ping", description: "ping", inputSchema: { properties: {} }, handler: async () => "pong" }],
		})) {
			msgs.push(msg);
		}
		assert.equal(liveCalls, 2, `expected 2 live LLM calls, got ${liveCalls}`);
		assert.ok(msgs.some((m) => m.type === "system" && m.subtype === "max_turns"));
	});
});

describe("applyExtraSampling", () => {
	it("maps topP/topK/stop for anthropic payloads", () => {
		const p = applyExtraSampling("anthropic-messages", {}, { topP: 0.9, topK: 5, stopSequences: ["END"] });
		assert.deepEqual(p, { top_p: 0.9, top_k: 5, stop_sequences: ["END"] });
	});
});

// ── 11: extends/dependency clones are reused ────────────────────────────

describe("syncGitRepo", () => {
	it("reuses an existing clone instead of failing", () => {
		const src = tmpRepo("gitagent-src-");
		const parent = mkdtempSync(join(tmpdir(), "gitagent-deps-"));
		const dest = join(parent, "src");
		assert.equal(syncGitRepo(src, dest, { cwd: parent }), true);
		assert.equal(syncGitRepo(src, dest, { cwd: parent }), true, "second run must reuse the clone");
		rmSync(src, { recursive: true, force: true });
		rmSync(parent, { recursive: true, force: true });
	});
});

// ── 12: model string parsing ────────────────────────────────────────────

describe("model parsing", () => {
	function agentWithModel(model: string): string {
		const dir = mkdtempSync(join(tmpdir(), "gitagent-model-"));
		writeFileSync(join(dir, "agent.yaml"), `spec_version: "0.1.0"\nname: m\nversion: 0.1.0\ndescription: d\nmodel:\n  preferred: "${model}"\ntools: []\nruntime:\n  max_turns: 5\n`);
		return dir;
	}

	it("throws a clear error for unknown models", async () => {
		const dir = agentWithModel("anthropic:no-such-model");
		await assert.rejects(() => loadAgent(dir), /Unknown model anthropic:no-such-model/);
		rmSync(dir, { recursive: true, force: true });
	});

	it("does not treat a bare @ in the model id as a base URL", async () => {
		const dir = agentWithModel("google-vertex:claude-x@20241022");
		await assert.rejects(() => loadAgent(dir), /Unknown model google-vertex:claude-x@20241022/);
		rmSync(dir, { recursive: true, force: true });
	});

	it("accepts @https:// base URL overrides", async () => {
		const dir = agentWithModel("custom:my-model@https://example.invalid/v1");
		const loaded = await loadAgent(dir);
		assert.equal(loaded.model.id, "my-model");
		assert.equal(loaded.model.baseUrl, "https://example.invalid/v1");
		rmSync(dir, { recursive: true, force: true });
	});
});

// ── 14: long scheduler delays ───────────────────────────────────────────

describe("scheduleAt", () => {
	it("does not fire early for delays beyond the setTimeout limit", () => {
		mock.timers.enable({ apis: ["setTimeout", "Date"], now: 0 });
		try {
			let fired = false;
			scheduleAt(MAX_TIMEOUT_MS * 2 + 1000, () => { fired = true; }, () => {});
			mock.timers.tick(MAX_TIMEOUT_MS);
			assert.equal(fired, false);
			mock.timers.tick(MAX_TIMEOUT_MS);
			assert.equal(fired, false);
			mock.timers.tick(1000);
			assert.equal(fired, true);
		} finally {
			mock.timers.reset();
		}
	});
});

// ── 17: sandbox ~ ──────────────────────────────────────────────────────

describe("resolveSandboxPath", () => {
	it("maps ~ to the sandbox home, not the host home", () => {
		assert.equal(resolveSandboxPath("~/notes.md", "/home/user/repo"), "/home/user/notes.md");
		assert.equal(resolveSandboxPath("a.txt", "/home/user/repo"), "/home/user/repo/a.txt");
	});
});
