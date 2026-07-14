import { estimateTokens } from "../compact.js";

export interface DocChunk {
	id: string;
	title: string;
	content: string;
	estimatedTokens: number;
}

export interface StoredDoc {
	filePath: string;
	totalChunks: number;
	chunks: DocChunk[];
	lastAccessed: number;
}

export interface FetchResult {
	content: string;
	tokens: number;
	has_refs: string[];
}

// Soft cap: evict LRU docs when total in-memory chars exceeds this limit.
// Prevents OOM in long sessions reading many large docs.
const MAX_STORE_CHARS = 10 * 1024 * 1024; // 10 MB

// Per-session in-memory store for CCR (reversible compression).
// Keyed by file path — survives multiple read calls in a session.
export class DocStore {
	private docs = new Map<string, StoredDoc>();
	private totalChars = 0;

	store(filePath: string, chunks: DocChunk[]): void {
		// Evict existing entry's char count before overwriting
		const existing = this.docs.get(filePath);
		if (existing) {
			this.totalChars -= existing.chunks.reduce((s, c) => s + c.content.length, 0);
		}

		const newChars = chunks.reduce((s, c) => s + c.content.length, 0);
		this.docs.set(filePath, { filePath, totalChunks: chunks.length, chunks, lastAccessed: Date.now() });
		this.totalChars += newChars;
		this.evictIfNeeded();
	}

	fetch(filePath: string, chunkId: string): FetchResult | null {
		const doc = this.docs.get(filePath);
		if (!doc) return null;
		const chunk = doc.chunks.find((c) => c.id === chunkId);
		if (!chunk) return null;

		doc.lastAccessed = Date.now();

		// Extract section IDs explicitly referenced as [sN] in this chunk's content.
		// Require bracket syntax to avoid false positives from words like "session3".
		const has_refs = [...chunk.content.matchAll(/\[s(\d+)\]/g)]
			.map((m) => `s${m[1]}`)
			.filter((id) => id !== chunkId && doc.chunks.some((c) => c.id === id));

		return { content: chunk.content, tokens: chunk.estimatedTokens, has_refs: [...new Set(has_refs)] };
	}

	getOutline(filePath: string): string | null {
		const doc = this.docs.get(filePath);
		if (!doc) return null;
		return doc.chunks
			.map((c) => {
				const snippet = c.content.split("\n").find((l) => l.trim().length > 20)?.trim() ?? "";
				const preview = snippet.length > 80 ? snippet.slice(0, 77) + "…" : snippet;
				return `[${c.id}] ${c.title} (~${c.estimatedTokens} tokens)${preview ? ` — "${preview}"` : ""}`;
			})
			.join("\n");
	}

	has(filePath: string): boolean {
		return this.docs.has(filePath);
	}

	private evictIfNeeded(): void {
		if (this.totalChars <= MAX_STORE_CHARS) return;
		// Evict least-recently-accessed docs until under the cap
		const sorted = [...this.docs.entries()].sort((a, b) => a[1].lastAccessed - b[1].lastAccessed);
		for (const [key, doc] of sorted) {
			if (this.totalChars <= MAX_STORE_CHARS) break;
			this.totalChars -= doc.chunks.reduce((s, c) => s + c.content.length, 0);
			this.docs.delete(key);
		}
	}
}

// Detect headings in flat PDF text (no markdown ## markers).
function detectFlatHeading(line: string): string | null {
	const t = line.trim();
	if (!t || t.length > 120) return null;
	// "Section Title\t1." — common in PDFs where number is right-aligned
	const tabNum = t.match(/^(.+?)\t\d+\.?$/);
	if (tabNum && tabNum[1].trim().length > 2) return tabNum[1].trim();
	// ALL CAPS words (no lowercase), short enough to be a heading
	if (t.length <= 80 && /^[A-Z][A-Z\s\-&/(),.:0-9]{2,}$/.test(t) && /[A-Z]{2}/.test(t)) return t;
	// Numbered section: "1.", "2.1 Title", "Section 3:"
	if (/^(\d+\.)+\s+\S/.test(t)) return t;
	if (/^(section|chapter|part|article)\s+\d+/i.test(t)) return t;
	return null;
}

// Split document into sections. Uses markdown headings if present,
// falls back to heuristic detection for flat PDF text,
// then to fixed-size chunks as last resort.
export function chunkDocument(markdown: string): DocChunk[] {
	const lines = markdown.split("\n");

	// ── Pass 1: markdown headings ──────────────────────────────────────
	const mdSections: { title: string; lines: string[] }[] = [];
	let cur: { title: string; lines: string[] } | null = null;
	for (const line of lines) {
		const h = line.match(/^(#{1,3})\s+(.+)/);
		if (h) {
			if (cur) mdSections.push(cur);
			cur = { title: h[2].trim(), lines: [line] };
		} else {
			if (!cur) cur = { title: "Preamble", lines: [] };
			cur.lines.push(line);
		}
	}
	if (cur?.lines.length) mdSections.push(cur);

	if (mdSections.length > 2) return toChunks(mdSections);

	// ── Pass 2: heuristic heading detection for flat PDF text ──────────
	const flatSections: { title: string; lines: string[] }[] = [];
	cur = null;
	for (let i = 0; i < lines.length; i++) {
		const line = lines[i];
		const prev = lines[i - 1]?.trim() ?? "";
		const next = lines[i + 1]?.trim() ?? "";
		const heading = (!prev || !next) ? detectFlatHeading(line) : null;
		if (heading) {
			if (cur) flatSections.push(cur);
			cur = { title: heading, lines: [] };
		} else {
			if (!cur) cur = { title: "Preamble", lines: [] };
			cur.lines.push(line);
		}
	}
	if (cur?.lines.length) flatSections.push(cur);

	if (flatSections.length > 2) return toChunks(flatSections);

	// ── Pass 3: fixed-size fallback (~2K tokens per chunk) ────────────
	const CHUNK_CHARS = 8000;
	const chunks: DocChunk[] = [];
	let offset = 0;
	let idx = 1;
	while (offset < markdown.length) {
		const content = markdown.slice(offset, offset + CHUNK_CHARS).trim();
		// Derive a title from the first non-empty line of this chunk
		const firstLine = content.split("\n").find((l) => l.trim())?.trim() ?? `Part ${idx}`;
		const title = firstLine.length > 60 ? firstLine.slice(0, 57) + "…" : firstLine;
		chunks.push({ id: `s${idx++}`, title, content, estimatedTokens: estimateTokens(content) });
		offset += CHUNK_CHARS;
	}
	return chunks;
}

const MAX_CHUNK_TOKENS = 5000;
const MAX_CHUNK_CHARS = MAX_CHUNK_TOKENS * 4;

function toChunks(sections: { title: string; lines: string[] }[]): DocChunk[] {
	const raw = sections
		.filter((s) => s.lines.join("\n").trim().length > 0)
		.map((s) => ({ title: s.title, content: s.lines.join("\n").trim() }));

	const out: DocChunk[] = [];
	let idx = 1;
	for (const s of raw) {
		if (s.content.length <= MAX_CHUNK_CHARS) {
			out.push({ id: `s${idx++}`, title: s.title, content: s.content, estimatedTokens: estimateTokens(s.content) });
		} else {
			// Split oversized section into fixed sub-chunks
			let offset = 0;
			let sub = 1;
			while (offset < s.content.length) {
				const slice = s.content.slice(offset, offset + MAX_CHUNK_CHARS).trim();
				out.push({
					id: `s${idx++}`,
					title: sub === 1 ? s.title : `${s.title} (cont. ${sub})`,
					content: slice,
					estimatedTokens: estimateTokens(slice),
				});
				offset += MAX_CHUNK_CHARS;
				sub++;
			}
		}
	}
	return out;
}
