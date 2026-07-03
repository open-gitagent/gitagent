import type { OutputVerdict } from "./types.js";

const isTTY = Boolean(process.stdout.isTTY);
const dim   = (s: string) => isTTY ? `\x1b[2m${s}\x1b[0m` : s;
const bold  = (s: string) => isTTY ? `\x1b[1m${s}\x1b[0m` : s;
const green = (s: string) => isTTY ? `\x1b[32m${s}\x1b[0m` : s;
const red   = (s: string) => isTTY ? `\x1b[31m${s}\x1b[0m` : s;

const CATEGORY_COLORS: Record<string, (s: string) => string> = {
	format:     (s) => `\x1b[34m${s}\x1b[0m`,
	content:    (s) => `\x1b[32m${s}\x1b[0m`,
	quality:    (s) => `\x1b[35m${s}\x1b[0m`,
	constraint: (s) => `\x1b[31m${s}\x1b[0m`,
	behavior:   (s) => `\x1b[36m${s}\x1b[0m`,
	tone:       (s) => `\x1b[33m${s}\x1b[0m`,
};

function colorCategory(cat: string): string {
	return (CATEGORY_COLORS[cat] ?? ((s: string) => s))(cat);
}

export function displayOutputReport(verdict: OutputVerdict, attempts: number): void {
	const line = "─".repeat(50);
	console.log("");
	console.log(bold(`  Brief Evaluation — ${attempts} attempt${attempts !== 1 ? "s" : ""}`));
	console.log(dim(`  ${line}`));

	for (const r of verdict.results) {
		const icon = r.passed ? green("✓") : red("✗");
		const cat  = colorCategory(r.category);
		const assertion = r.assertion.length > 55 ? r.assertion.slice(0, 54) + "…" : r.assertion;
		console.log(`  ${icon}  ${r.assertion_id.toString().padStart(2)}. [${cat}] ${assertion}`);
		if (!r.passed && r.evidence) {
			console.log(dim(`        Evidence: ${r.evidence}`));
		}
	}

	console.log(dim(`  ${line}`));

	const total = verdict.results.length;
	const score = total > 0 ? Math.round((verdict.passed_count / total) * 100) : 100;
	const passFail = verdict.all_passed
		? green(`  ${verdict.passed_count}/${total} passed`)
		: red(`  ${verdict.passed_count}/${total} passed`) + dim(` · `) + red(`${verdict.failed_count} failed`);

	console.log(`${passFail}  ·  Score: ${score}/100`);
	if (verdict.summary) {
		console.log(dim(`  ${verdict.summary}`));
	}
	console.log("");
}
