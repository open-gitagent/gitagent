/**
 * CodeCompressor — strips noise from source code before the LLM sees it.
 *
 * When an agent reads code files, most of the token cost is comments,
 * docstrings, blank lines, and decorative whitespace. The LLM needs the
 * structure and logic — not the annotations.
 *
 * Techniques applied:
 * 1. Strip single-line comments (// and #)
 * 2. Strip block comments (/* ... *\/ and """ ... """)
 * 3. Collapse multiple blank lines into one
 * 4. Trim trailing whitespace per line
 *
 * Does NOT strip:
 * - String literals (could contain // or # that look like comments)
 * - Type annotations (useful signal for the LLM)
 * - Import statements
 */

export type Language = "ts" | "js" | "py" | "go" | "rust" | "java" | "cpp" | "unknown";

const EXTENSION_MAP: Record<string, Language> = {
	".ts": "ts",
	".tsx": "ts",
	".js": "js",
	".jsx": "js",
	".py": "py",
	".go": "go",
	".rs": "rust",
	".java": "java",
	".cpp": "cpp",
	".cc": "cpp",
	".c": "cpp",
	".h": "cpp",
};

export function detectLanguage(filename: string): Language {
	const ext = filename.slice(filename.lastIndexOf(".")).toLowerCase();
	return EXTENSION_MAP[ext] ?? "unknown";
}

function stripSlashComments(code: string): string {
	// Remove // comments but preserve URLs (https://) and protocol strings
	// Strategy: only strip if // is preceded by whitespace or start-of-line
	return code
		.split("\n")
		.map((line) => {
			// Find // that isn't inside a string
			let inString: string | null = null;
			for (let i = 0; i < line.length - 1; i++) {
				const ch = line[i];
				if (inString) {
					if (ch === inString && line[i - 1] !== "\\") inString = null;
				} else {
					if (ch === '"' || ch === "'" || ch === "`") { inString = ch; continue; }
					if (ch === "/" && line[i + 1] === "/") {
						return line.slice(0, i).trimEnd();
					}
				}
			}
			return line;
		})
		.join("\n");
}

function stripHashComments(code: string): string {
	return code
		.split("\n")
		.map((line) => {
			let inString: string | null = null;
			for (let i = 0; i < line.length; i++) {
				const ch = line[i];
				if (inString) {
					if (ch === inString && line[i - 1] !== "\\") inString = null;
				} else {
					if (ch === '"' || ch === "'") { inString = ch; continue; }
					if (ch === "#") return line.slice(0, i).trimEnd();
				}
			}
			return line;
		})
		.join("\n");
}

function stripBlockComments(code: string): string {
	// Remove /* ... */ block comments
	return code.replace(/\/\*[\s\S]*?\*\//g, "");
}

function stripPythonDocstrings(code: string): string {
	// Remove triple-quoted strings that appear as standalone statements (docstrings)
	// Matches """ or ''' docstrings at the start of a block
	return code
		.replace(/^(\s*)"""[\s\S]*?"""/gm, "")
		.replace(/^(\s*)'''[\s\S]*?'''/gm, "");
}

function collapseBlankLines(code: string): string {
	// Replace 3+ consecutive blank lines with a single blank line
	return code.replace(/\n{3,}/g, "\n\n");
}

function trimTrailingWhitespace(code: string): string {
	return code
		.split("\n")
		.map((l) => l.trimEnd())
		.join("\n");
}

function estimateTokens(s: string): number {
	return Math.ceil(s.length / 4);
}

export interface CompressionResult {
	compressed: string;
	originalTokens: number;
	compressedTokens: number;
	reductionPct: number;
	language: Language;
}

/**
 * Compress source code by removing comments and noise.
 * Returns the original if the file language is unknown or compression doesn't help.
 */
export function compressCode(text: string, filename: string): CompressionResult {
	const language = detectLanguage(filename);
	const originalTokens = estimateTokens(text);

	if (language === "unknown") {
		return { compressed: text, originalTokens, compressedTokens: originalTokens, reductionPct: 0, language };
	}

	let result = text;

	if (language === "py") {
		result = stripPythonDocstrings(result);
		result = stripHashComments(result);
	} else if (language === "ts" || language === "js") {
		result = stripBlockComments(result);
		result = stripSlashComments(result);
	} else if (language === "go" || language === "rust" || language === "java" || language === "cpp") {
		result = stripBlockComments(result);
		result = stripSlashComments(result);
	}

	result = collapseBlankLines(result);
	result = trimTrailingWhitespace(result);
	result = result.trim();

	const compressedTokens = estimateTokens(result);

	if (compressedTokens >= originalTokens) {
		return { compressed: text, originalTokens, compressedTokens: originalTokens, reductionPct: 0, language };
	}

	const reductionPct = Math.round(((originalTokens - compressedTokens) / originalTokens) * 100);
	return { compressed: result, originalTokens, compressedTokens, reductionPct, language };
}

/**
 * Returns true for file extensions the compressor handles.
 */
export function isSourceFile(filename: string): boolean {
	return detectLanguage(filename) !== "unknown";
}
