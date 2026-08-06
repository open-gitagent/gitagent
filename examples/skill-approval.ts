/**
 * Unreliable-skill handling in SDK mode — runnable example.
 *
 * Scaffolds a throwaway agent whose single skill is deliberately flagged as
 * unreliable (confidence 0.3), then runs a task that matches it. One option
 * decides what happens:
 *
 *   autoRepair: false (default) → the agent is told the skill is unreliable and
 *                                 repair is refused; SKILL.md is never touched.
 *   autoRepair: true            → the agent rewrites the skill's steps from its
 *                                 own recorded failures, commits, then uses it.
 *
 * Usage (from the repo root, after `npm run build`):
 *
 *   node --experimental-strip-types examples/skill-approval.ts        # default: no repair
 *   node --experimental-strip-types examples/skill-approval.ts --auto # autoRepair: true
 *
 * Requires ANTHROPIC_API_KEY, or set GITAGENT_MODEL=provider:model plus that
 * provider's key.
 *
 * (In the CLI — `gitagent --dir .` on a TTY — you instead get interactive
 * prompts: proceed/repair/skip, then accept/edit/cancel on the diff.)
 */

import { mkdtempSync, mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { execSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { query } from "../dist/exports.js";

const MODEL = process.env.GITAGENT_MODEL || "anthropic:claude-sonnet-4-6";
const AUTO_REPAIR = process.argv.includes("--auto");

// ── Scaffold a throwaway agent with one flagged skill ───────────────────

function scaffoldAgent(): string {
	const dir = mkdtempSync(join(tmpdir(), "skill-approval-"));

	writeFileSync(join(dir, "agent.yaml"), `spec_version: "0.1.0"
name: skill-approval-demo
version: 0.1.0
description: Demo agent for unreliable-skill handling

model:
  preferred: "${MODEL}"
  fallback: []

tools: [cli, read, write, memory]

runtime:
  max_turns: 12
  timeout: 120
`);

	// The fixture: a skill that has failed more than it has worked, plus the
	// failure lessons a repair would feed back to the model.
	const skillDir = join(dir, "skills", "laptop-setup-checklist");
	mkdirSync(skillDir, { recursive: true });
	writeFileSync(join(skillDir, "SKILL.md"), `---
name: laptop-setup-checklist
description: Generate a new laptop setup checklist covering OS updates, essential software installation, and backup/security configuration, saved as a Markdown file in the workspace directory.
confidence: 0.3
usage_count: 7
success_count: 2
failure_count: 5
negative_examples:
  - Saved the checklist outside the workspace directory so the user could not find it
  - Assumed macOS and emitted steps that are invalid on Windows
---

## Steps
1. Identify the workspace directory.
2. Write a checklist file covering OS updates, software and backups.
3. Tell the user it is done.
`);

	mkdirSync(join(dir, "workspace"), { recursive: true });

	// A git repo so a repair commit actually lands and you can read it back.
	execSync("git init -q && git add -A && git commit -q -m fixture --no-gpg-sign", {
		cwd: dir,
		stdio: "pipe",
		env: {
			...process.env,
			GIT_AUTHOR_NAME: "demo", GIT_AUTHOR_EMAIL: "demo@example.com",
			GIT_COMMITTER_NAME: "demo", GIT_COMMITTER_EMAIL: "demo@example.com",
		},
	});

	return dir;
}

// ── Run it ──────────────────────────────────────────────────────────────

async function main() {
	const dir = scaffoldAgent();
	console.log(`agent dir:  ${dir}`);
	console.log(`model:      ${MODEL}`);
	console.log(`autoRepair: ${AUTO_REPAIR}\n`);

	for await (const msg of query({
		prompt:
			"Help me set up a new laptop. Start by tracking this with task_tracker " +
			'(objective: "Help the user set up a new laptop checklist"), then follow whatever it tells you to do.',
		dir,
		model: MODEL,
		autoRepair: AUTO_REPAIR, // ← the whole knob
	})) {
		switch (msg.type) {
			case "delta":
				if (msg.deltaType === "text") process.stdout.write(msg.content);
				break;
			case "tool_use":
				console.log(`\n▶ ${msg.toolName}(${JSON.stringify(msg.args).slice(0, 120)})`);
				break;
			case "tool_result": {
				const text = msg.content.length > 500 ? `${msg.content.slice(0, 500)}…` : msg.content;
				console.log(text.split("\n").map((l) => `  ${l}`).join("\n"));
				break;
			}
			case "assistant":
				if (msg.errorMessage) console.error(`\n[error] ${msg.errorMessage}`);
				break;
			case "system":
				console.log(`[${msg.subtype}] ${msg.content}`);
				break;
		}
	}

	// ── What actually happened to the skill on disk ─────────────────────
	const skillFile = join(dir, "skills", "laptop-setup-checklist", "SKILL.md");
	const after = readFileSync(skillFile, "utf-8");
	const confidence = after.match(/^confidence: (.+)$/m)?.[1];
	const repairs = after.match(/^repair_count: (.+)$/m)?.[1] ?? "0";
	const log = execSync("git log --oneline", { cwd: dir, encoding: "utf-8" }).trim();

	console.log(`\n──────── result ────────`);
	// 0.3 = never repaired. A repair resets it to 0.6, and a successful run nudges it up.
	console.log(`confidence:   ${confidence}   (0.3 = untouched, >= 0.6 = repaired)`);
	console.log(`repair_count: ${repairs}`);
	console.log(`git log:\n${log.split("\n").map((l) => `  ${l}`).join("\n")}`);
	console.log(`\nInspect the skill:  cat ${skillFile}`);
	console.log(`Inspect the output: ls ${join(dir, "workspace")}`);
}

main().catch((err) => {
	console.error(err);
	process.exit(1);
});
