import { writeFile, readFile, unlink } from "fs/promises";
import { join } from "path";
import { tmpdir } from "os";
import { spawnSync } from "child_process";
import { parse as yamlParse, stringify as yamlStringify } from "yaml";
import type { BriefDraft, AssertionCategory, ComplexityLevel } from "./types.js";

const VALID_CATEGORIES: AssertionCategory[] = ["format", "content", "quality", "constraint", "behavior", "tone"];
const VALID_COMPLEXITIES: ComplexityLevel[] = ["low", "medium", "high"];

const EDIT_FILE_HEADER = `# Agent Brief — Edit Mode
# ─────────────────────────────────────────────────────────────────────
# Edit assertions, rubric, or ambiguities below. Save and close to apply.
#
# Rules for assertions:
#   • Each assertion must be binary — either passes or fails, no partial credit
#   • No vague words: good, appropriate, clear, sufficient, correct
#   • category must be one of: format, content, quality, constraint, behavior, tone
#   • test field must describe exactly HOW to verify (not just "read the output")
#
# At least 1 assertion each of: format, content, constraint
# ─────────────────────────────────────────────────────────────────────

`;

function serializeToEditYaml(draft: BriefDraft): string {
	const editData = {
		task_summary: draft.task_summary,
		estimated_complexity: draft.estimated_complexity,
		recommended_max_turns: draft.recommended_max_turns,
		assertions: draft.assertions.map(a => ({
			id: a.id,
			category: a.category,
			assertion: a.assertion,
			why: a.why,
			test: a.test,
		})),
		rubric: {
			craft: draft.rubric.craft,
			originality: draft.rubric.originality,
			tone: draft.rubric.tone,
			completeness: draft.rubric.completeness,
		},
		constraints_applied: draft.constraints_applied,
		ambiguities: draft.ambiguities,
	};

	return EDIT_FILE_HEADER + yamlStringify(editData, { lineWidth: 100, defaultStringType: "PLAIN" });
}

function parseEditYaml(content: string): BriefDraft {
	// Strip comment lines before parsing (yaml library handles # comments but be safe)
	const parsed = yamlParse(content);

	if (!parsed || typeof parsed !== "object") {
		throw new Error("YAML parse returned empty or non-object result.");
	}

	return {
		task_summary: String(parsed.task_summary ?? ""),
		estimated_complexity: parsed.estimated_complexity as ComplexityLevel ?? "medium",
		recommended_max_turns: Number(parsed.recommended_max_turns ?? 10),
		assertions: Array.isArray(parsed.assertions)
			? parsed.assertions.map((a: any, idx: number) => ({
				id: Number(a.id ?? idx + 1),
				category: String(a.category ?? "content") as AssertionCategory,
				assertion: String(a.assertion ?? ""),
				why: String(a.why ?? ""),
				test: String(a.test ?? ""),
			}))
			: [],
		rubric: {
			craft: String(parsed.rubric?.craft ?? ""),
			originality: String(parsed.rubric?.originality ?? ""),
			tone: String(parsed.rubric?.tone ?? ""),
			completeness: String(parsed.rubric?.completeness ?? ""),
		},
		constraints_applied: Array.isArray(parsed.constraints_applied)
			? parsed.constraints_applied.map(String)
			: [],
		ambiguities: Array.isArray(parsed.ambiguities)
			? parsed.ambiguities.map(String)
			: [],
	};
}

export interface ValidationResult {
	valid: boolean;
	errors: string[];
	warnings: string[];
}

export function validateEditedDraft(draft: BriefDraft): ValidationResult {
	const errors: string[] = [];
	const warnings: string[] = [];

	if (!draft.assertions || draft.assertions.length === 0) {
		errors.push("At least 1 assertion is required.");
	}

	if (!draft.task_summary || draft.task_summary.trim() === "") {
		errors.push("task_summary cannot be empty.");
	}

	if (!VALID_COMPLEXITIES.includes(draft.estimated_complexity)) {
		errors.push(`estimated_complexity must be one of: ${VALID_COMPLEXITIES.join(", ")}`);
	}

	for (const a of draft.assertions) {
		if (!a.assertion || a.assertion.trim() === "") {
			errors.push(`Assertion ${a.id}: assertion text cannot be empty.`);
		}
		if (!a.test || a.test.trim() === "") {
			errors.push(`Assertion ${a.id}: test field cannot be empty.`);
		}
		if (!VALID_CATEGORIES.includes(a.category)) {
			errors.push(`Assertion ${a.id}: category "${a.category}" is invalid. Must be one of: ${VALID_CATEGORIES.join(", ")}`);
		}
	}

	// Warnings (non-blocking)
	const categories = new Set(draft.assertions.map(a => a.category));
	if (!categories.has("format"))     warnings.push("No format assertion present. Consider adding one (structure, length, shape of output).");
	if (!categories.has("content"))    warnings.push("No content assertion present. Consider adding one (what must be included).");
	if (!categories.has("constraint")) warnings.push("No constraint assertion present. Consider adding one from RULES.md.");

	const rubricFields = ["craft", "originality", "tone", "completeness"] as const;
	for (const field of rubricFields) {
		if (!draft.rubric[field] || draft.rubric[field].trim() === "") {
			warnings.push(`Rubric field "${field}" is empty.`);
		}
	}

	return { valid: errors.length === 0, errors, warnings };
}

export async function openInEditor(draft: BriefDraft): Promise<BriefDraft | null> {
	const tmpFile = join(tmpdir(), `gitagent-brief-edit-${Date.now()}.yaml`);

	const isTTY  = Boolean(process.stdout.isTTY);
	const red    = (s: string) => isTTY ? `\x1b[31m${s}\x1b[0m` : s;
	const yellow = (s: string) => isTTY ? `\x1b[33m${s}\x1b[0m` : s;

	try {
		await writeFile(tmpFile, serializeToEditYaml(draft), "utf-8");
	} catch (err: any) {
		console.error(red(`[brief] Failed to write temp file: ${err.message}`));
		return null;
	}

	const editor = process.env.VISUAL || process.env.EDITOR || "vi";

	const spawnResult = spawnSync(editor, [tmpFile], { stdio: "inherit" });

	if (spawnResult.error) {
		console.error(red(`[brief] Failed to launch editor "${editor}" (${spawnResult.error.message}). Set $EDITOR or $VISUAL to a valid editor.`));
		await unlink(tmpFile).catch(() => {});
		return null;
	}
	if (spawnResult.status !== 0 && spawnResult.status !== null) {
		console.error(yellow(`[brief] Editor exited with status ${spawnResult.status} — if you didn't save, your changes were not applied.`));
		// Still try to read the file below — the user may have saved before a non-zero exit.
	}

	let content: string;
	try {
		content = await readFile(tmpFile, "utf-8");
	} catch {
		console.error(red("[brief] Could not read edited file."));
		await unlink(tmpFile).catch(() => {});
		return null;
	}

	await unlink(tmpFile).catch(() => {});

	let parsed: BriefDraft;
	try {
		parsed = parseEditYaml(content);
	} catch (err: any) {
		console.error(red(`[brief] YAML parse error: ${err.message}`));
		return null;
	}

	const validation = validateEditedDraft(parsed);

	// Show warnings even if valid
	if (validation.warnings.length > 0) {
		for (const w of validation.warnings) {
			console.log(yellow(`  ⚠ ${w}`));
		}
	}

	if (!validation.valid) {
		for (const e of validation.errors) {
			console.error(red(`  ✗ ${e}`));
		}
		return null;
	}

	return parsed;
}
