import { readFile, writeFile, mkdir, stat } from "fs/promises";
import { resolve, basename, join } from "path";
import { homedir } from "os";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { readSchema, MAX_LINES, paginateLines } from "./shared.js";
import { crushJson, isJson, compressCode, isSourceFile } from "../compression/index.js";
import { convertToMarkdown, isConvertible } from "./doc-converter.js";
import { DocStore, chunkDocument } from "./doc-store.js";
import type { ConversionError } from "./doc-converter.js";
import type { CostTracker } from "../cost-tracker.js";

const CCR_THRESHOLD_TOKENS = 8000;
const DOC_CACHE_DIR = "memory/.doc-cache";

function resolvePath(path: string, cwd: string): string {
	if (path.startsWith("~/") || path === "~") {
		path = homedir() + path.slice(1);
	}
	return path.startsWith("/") ? path : resolve(cwd, path);
}

function isBinary(buffer: Buffer): boolean {
	// Check first 8KB for null bytes
	const check = buffer.subarray(0, 8192);
	for (let i = 0; i < check.length; i++) {
		if (check[i] === 0) return true;
	}
	return false;
}

function estimateTokens(s: string): number {
	return Math.ceil(s.length / 4);
}

function cacheKey(absolutePath: string): string {
	// Use a hash to avoid collisions from paths that differ only in separators
	// e.g. /tmp/a/b.pdf and /tmp/a_b.pdf would both map to _tmp_a_b.pdf
	let hash = 0;
	for (let i = 0; i < absolutePath.length; i++) {
		hash = (Math.imul(31, hash) + absolutePath.charCodeAt(i)) >>> 0;
	}
	const safe = absolutePath.replace(/[/\\:.]/g, "_").slice(-60);
	return `${safe}_${hash.toString(16)}.md`;
}

async function loadFromCache(cwd: string, absolutePath: string): Promise<string | null> {
	try {
		const key = cacheKey(absolutePath);
		const cachePath = join(cwd, DOC_CACHE_DIR, key);
		const [cacheStat, sourceStat] = await Promise.all([
			stat(cachePath).catch(() => null),
			stat(absolutePath).catch(() => null),
		]);
		if (!cacheStat || !sourceStat) return null;
		// Invalidate if source is newer than cache.
		// Known edge case: a file replaced with identical content on some filesystems
		// may share the same mtime but have a different inode — cache won't invalidate.
		// Acceptable for v1; document-level GC handles stale entries.
		if (sourceStat.mtimeMs > cacheStat.mtimeMs) return null;
		return await readFile(cachePath, "utf-8");
	} catch {
		return null;
	}
}

async function saveToCache(cwd: string, absolutePath: string, markdown: string): Promise<void> {
	try {
		const cacheDir = join(cwd, DOC_CACHE_DIR);
		await mkdir(cacheDir, { recursive: true });
		const key = cacheKey(absolutePath);
		await writeFile(join(cacheDir, key), markdown, "utf-8");
	} catch {
		// Cache write failure is non-fatal
	}
}

export function createReadTool(
	cwd: string,
	costTracker?: CostTracker,
	docStore?: DocStore,
): AgentTool<typeof readSchema> {
	return {
		name: "read",
		label: "read",
		description: `Read the contents of a file. Output is limited to ${MAX_LINES} lines or ~100KB. Use offset/limit for large files. Supports PDF, DOCX, XLSX, PPTX — converted to markdown automatically.`,
		parameters: readSchema,
		execute: async (
			_toolCallId: string,
			{ path, offset, limit, mode }: { path: string; offset?: number; limit?: number; mode?: "outline" },
			signal?: AbortSignal,
		) => {
			if (signal?.aborted) throw new Error("Operation aborted");

			const absolutePath = resolvePath(path, cwd);

			// ── mode="outline": dry-run for CCR docs ──────────────────────
			// Returns outline + token estimates without loading content.
			// If doc is already chunked in store, return cached outline.
			// Otherwise convert and chunk now so the outline is accurate.
			if (mode === "outline") {
				if (docStore?.has(absolutePath)) {
					return {
						content: [{ type: "text", text: `[Outline for ${path}]\n\n${docStore.getOutline(absolutePath)}` }],
						details: undefined,
					};
				}
				// Not yet in store — convert and chunk to produce the outline
				const buf = await readFile(absolutePath);
				if (isConvertible(path)) {
					let markdown = await loadFromCache(cwd, absolutePath);
					if (!markdown) {
						const conv = await convertToMarkdown(absolutePath, buf);
						if (!conv || "error" in conv) {
							return {
								content: [{ type: "text", text: `[Cannot produce outline: ${"error" in (conv ?? {}) ? (conv as any).error : "conversion failed"}]` }],
								details: undefined,
							};
						}
						markdown = conv.markdown;
						await saveToCache(cwd, absolutePath, markdown);
					}
					if (docStore && estimateTokens(markdown) > CCR_THRESHOLD_TOKENS) {
						const chunks = chunkDocument(markdown);
						docStore.store(absolutePath, chunks);
						return {
							content: [{ type: "text", text: `[Outline for ${path}]\n\n${docStore.getOutline(absolutePath)}` }],
							details: undefined,
						};
					}
					return {
						content: [{ type: "text", text: `[${path} is ${estimateTokens(markdown)} tokens — below CCR threshold, no outline needed]` }],
						details: undefined,
					};
				}
				return {
					content: [{ type: "text", text: `[${path} is not a convertible format — outline not available]` }],
					details: undefined,
				};
			}

			const buffer = await readFile(absolutePath);

			// ── Stage 1: Document conversion ──────────────────────────────
			// Check extension FIRST — some PDFs have no null bytes in the
			// first 8KB so isBinary() returns false, but they're still not
			// readable as plain text. Extension is the reliable signal.
			if (isConvertible(path)) {

				// Check cache first — skip conversion if cached copy is fresh
				let markdown = await loadFromCache(cwd, absolutePath);
				let fromCache = true;

				if (!markdown) {
					fromCache = false;
					const result = await convertToMarkdown(absolutePath, buffer);
					if (!result) {
						return {
							content: [{ type: "text", text: `[Binary file: ${path} (${buffer.length} bytes) — conversion failed]` }],
							details: undefined,
						};
					}
					if ("error" in result) {
						return {
							content: [{ type: "text", text: `[doc-converter error: ${(result as ConversionError).error}]` }],
							details: undefined,
						};
					}
					markdown = result.markdown;
					if (costTracker) costTracker.addConversion(result.savedTokens);
					await saveToCache(cwd, absolutePath, markdown);
				}

				const cacheNote = fromCache ? " (from cache)" : "";

				// ── Stage 2b: CCR chunking for large docs ──────────────────
				// If doc exceeds threshold and a docStore is available, return
				// an outline and let the agent fetch sections on demand.
				if (docStore && estimateTokens(markdown) > CCR_THRESHOLD_TOKENS) {
					const chunks = chunkDocument(markdown);
					docStore.store(absolutePath, chunks);
					const outline = docStore.getOutline(absolutePath)!;
					return {
						content: [{
							type: "text",
							text: `[${path}${cacheNote} — ${chunks.length} sections, too large to show in full]\n\nUse \`read_doc_section\` with a section ID to read any section.\n\n${outline}`,
						}],
						details: undefined,
					};
				}

				return {
					content: [{ type: "text", text: `[Converted from binary${cacheNote}]\n\n${markdown}` }],
					details: undefined,
				};
			}

			// ── Non-convertible binary files ──────────────────────────────
			if (isBinary(buffer)) {
				return {
					content: [{ type: "text", text: `[Binary file: ${path} (${buffer.length} bytes)]` }],
					details: undefined,
				};
			}

			// ── Plain text file ────────────────────────────────────────────
			const text = buffer.toString("utf-8");
			const page = paginateLines(text, offset, limit);
			let result = page.text;

			if (page.hasMore) {
				const nextOffset = page.shownRange[1] + 1;
				result += `\n\n[Showing lines ${page.shownRange[0]}-${page.shownRange[1]} of ${page.totalLines}. Use offset=${nextOffset} to continue.]`;
			}

			// ── Stage 2a: Compression passes ──────────────────────────────
			// Apply only on full reads (no offset/limit) to avoid corrupting
			// paginated output the user is navigating through.
			if (!offset && !limit) {
				const filename = basename(path);

				if (isJson(result)) {
					const { compressed, reductionPct } = crushJson(result);
					if (reductionPct > 0) {
						result = compressed + `\n\n[SmartCrusher: −${reductionPct}% tokens]`;
					}
				} else if (isSourceFile(filename)) {
					const { compressed, reductionPct, language } = compressCode(result, filename);
					if (reductionPct > 0) {
						result = compressed + `\n\n[CodeCompressor (${language}): −${reductionPct}% tokens]`;
					}
				}
			}

			return {
				content: [{ type: "text", text: result }],
				details: undefined,
			};
		},
	};
}
