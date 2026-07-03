const isTTY = Boolean(process.stdout.isTTY);

const ANSI: Record<string, string> = {
	format: "34",
	content: "32",
	quality: "35",
	constraint: "31",
	behavior: "36",
	tone: "33",
};

export const CATEGORY_COLORS: Record<string, (s: string) => string> = Object.fromEntries(
	Object.entries(ANSI).map(([category, code]) => [
		category,
		(s: string) => isTTY ? `\x1b[${code}m${s}\x1b[0m` : s,
	]),
);

export function colorCategory(cat: string): string {
	const fn = CATEGORY_COLORS[cat] ?? ((s: string) => s);
	return fn(cat);
}
