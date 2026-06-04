/**
 * Tests for the memory tool (src/tools/memory.ts).
 *
 * The memory tool provides git-backed persistent memory with load/save
 * operations. Each save creates a git commit, giving full history of
 * what the agent has remembered.
 */
import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile, mkdir } from "fs/promises";
import { join } from "path";
import { tmpdir } from "os";
import { execSync } from "child_process";

let createMemoryTool: typeof import("../../../dist/tools/memory.js").createMemoryTool;

before(async () => {
	const mod = await import("../../../dist/tools/memory.js");
	createMemoryTool = mod.createMemoryTool;
});

describe("memory tool", () => {
	/** Create a temporary directory with git init and return the path. */
	async function setupRepo(): Promise<string> {
		const dir = await mkdtemp(join(tmpdir(), "gitagent-memory-test-"));
		execSync("git init -q", { cwd: dir });
		// Configure git user for commits
		execSync('git config user.email "test@gitagent.test"', { cwd: dir });
		execSync('git config user.name "Test Agent"', { cwd: dir });
		return dir;
	}

	/** Clean up a temp directory. */
	async function cleanup(dir: string): Promise<void> {
		await rm(dir, { recursive: true, force: true }).catch(() => {});
	}

	// ── load ─────────────────────────────────────────────────────────

	describe("load", () => {
		it("returns stored memory content", async () => {
			const dir = await setupRepo();
			try {
				const tool = createMemoryTool(dir);

				// First, save some memory content
				await tool.execute("call-1", {
					action: "save",
					content: "# Memory\n\n- Remember to buy milk\n- Project uses TypeScript",
					message: "Initial memory",
				});

				// Now load it
				const result = await tool.execute("call-2", { action: "load" });

				assert.ok(result.content);
				assert.equal(result.content.length, 1);
				assert.ok(result.content[0].text.includes("Remember to buy milk"));
				assert.ok(result.content[0].text.includes("Project uses TypeScript"));
			} finally {
				await cleanup(dir);
			}
		});

		it("returns 'No memories yet.' when memory file is empty or missing", async () => {
			const dir = await setupRepo();
			try {
				const tool = createMemoryTool(dir);

				// Load from a repo with no memory file
				const result = await tool.execute("call-1", { action: "load" });

				assert.equal(result.content[0].text, "No memories yet.");
			} finally {
				await cleanup(dir);
			}
		});

		it("returns 'No memories yet.' when memory file has only heading", async () => {
			const dir = await setupRepo();
			try {
				// Write the default heading-only memory file
				await mkdir(join(dir, "memory"), { recursive: true });
				await writeFile(join(dir, "memory", "MEMORY.md"), "# Memory", "utf-8");

				const tool = createMemoryTool(dir);
				const result = await tool.execute("call-1", { action: "load" });

				assert.equal(result.content[0].text, "No memories yet.");
			} finally {
				await cleanup(dir);
			}
		});
	});

	// ── save ─────────────────────────────────────────────────────────

	describe("save", () => {
		it("writes content and commits to git", async () => {
			const dir = await setupRepo();
			try {
				const tool = createMemoryTool(dir);

				const result = await tool.execute("call-1", {
					action: "save",
					content: "# Memory\n\nSaved entry one.",
					message: "First save",
				});

				assert.equal(result.content.length, 1);
				assert.ok(
					result.content[0].text.includes("Memory saved and committed"),
				);
				assert.ok(result.content[0].text.includes("First save"));

				// Verify the file was written
				const { readFile } = await import("fs/promises");
				const fileContent = await readFile(
					join(dir, "memory", "MEMORY.md"),
					"utf-8",
				);
				assert.ok(fileContent.includes("Saved entry one"));

				// Verify the git commit exists
				const log = execSync("git log --oneline", {
					cwd: dir,
					encoding: "utf-8",
				});
				assert.ok(log.includes("First save"), `git log should contain commit: ${log}`);
			} finally {
				await cleanup(dir);
			}
		});

		it("uses default commit message when message is omitted", async () => {
			const dir = await setupRepo();
			try {
				const tool = createMemoryTool(dir);

				await tool.execute("call-1", {
					action: "save",
					content: "Memory without explicit message.",
				});

				const log = execSync("git log --oneline", {
					cwd: dir,
					encoding: "utf-8",
				});
				assert.ok(
					log.includes("Update memory"),
					`commit should default to "Update memory": ${log}`,
				);
			} finally {
				await cleanup(dir);
			}
		});

		it("requires content for save action", async () => {
			const dir = await setupRepo();
			try {
				const tool = createMemoryTool(dir);

				await assert.rejects(
					() =>
						tool.execute("call-1", {
							action: "save",
							// content intentionally omitted
						}),
					/content is required for save action/,
				);
			} finally {
				await cleanup(dir);
			}
		});
	});

	// ── abort signal ─────────────────────────────────────────────────

	describe("abort signal", () => {
		it("throws when signal is already aborted", async () => {
			const dir = await setupRepo();
			try {
				const tool = createMemoryTool(dir);
				const controller = new AbortController();
				controller.abort();

				await assert.rejects(
					() =>
						tool.execute("call-1", { action: "load" }, controller.signal),
					/Operation aborted/,
				);
			} finally {
				await cleanup(dir);
			}
		});
	});
});
