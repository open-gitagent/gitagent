// Minimal line diff for approval previews — no dependency, no patch format.
// Inputs here are skill step lists (tens of short lines), so a plain LCS table
// is fine; anything larger falls back to a whole-block replace.

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const red = (s: string) => `\x1b[31m${s}\x1b[0m`;
const green = (s: string) => `\x1b[32m${s}\x1b[0m`;

const MAX_LCS_LINES = 400;

export interface DiffLine {
	sign: " " | "-" | "+";
	text: string;
}

export function diffLines(before: string, after: string): DiffLine[] {
	const a = before.split(/\r?\n/);
	const b = after.split(/\r?\n/);

	if (a.length > MAX_LCS_LINES || b.length > MAX_LCS_LINES) {
		return [
			...a.map((text) => ({ sign: "-" as const, text })),
			...b.map((text) => ({ sign: "+" as const, text })),
		];
	}

	// lcs[i][j] = length of the longest common subsequence of a[i..] and b[j..]
	const lcs: number[][] = Array.from({ length: a.length + 1 }, () =>
		new Array(b.length + 1).fill(0),
	);
	for (let i = a.length - 1; i >= 0; i--) {
		for (let j = b.length - 1; j >= 0; j--) {
			lcs[i][j] = a[i] === b[j]
				? lcs[i + 1][j + 1] + 1
				: Math.max(lcs[i + 1][j], lcs[i][j + 1]);
		}
	}

	const out: DiffLine[] = [];
	let i = 0;
	let j = 0;
	while (i < a.length && j < b.length) {
		if (a[i] === b[j]) {
			out.push({ sign: " ", text: a[i] });
			i++;
			j++;
		} else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
			out.push({ sign: "-", text: a[i] });
			i++;
		} else {
			out.push({ sign: "+", text: b[j] });
			j++;
		}
	}
	while (i < a.length) out.push({ sign: "-", text: a[i++] });
	while (j < b.length) out.push({ sign: "+", text: b[j++] });

	return out;
}

export interface RenderDiffOptions {
	beforeLabel?: string;
	afterLabel?: string;
	indent?: string;
}

/** Colourised diff for terminal approval prompts. */
export function renderDiff(before: string, after: string, opts: RenderDiffOptions = {}): string {
	const indent = opts.indent ?? "  ";
	const lines = diffLines(before, after)
		.filter((l) => !(l.text === "" && l.sign === " "))
		.map((l) => {
			const text = `${indent}${l.sign} ${l.text}`;
			if (l.sign === "-") return red(text);
			if (l.sign === "+") return green(text);
			return dim(text);
		});

	const header = [
		dim(`${indent}--- ${opts.beforeLabel ?? "before"}`),
		dim(`${indent}+++ ${opts.afterLabel ?? "after"}`),
	];

	return [...header, ...lines].join("\n");
}
