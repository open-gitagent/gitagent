import { extname, basename } from "path";
import { estimateTokens } from "../compact.js";
import { PDFParse } from "pdf-parse";
import mammoth from "mammoth";
import * as XLSX from "xlsx";
import JSZip from "jszip";

export interface ConversionResult {
	markdown: string;
	originalTokens: number;
	convertedTokens: number;
	savedTokens: number;
	savedPercent: number;
}

// .doc and .ppt are legacy binary formats — mammoth/JSZip can't reliably parse them.
// Only the modern XML-based formats (.docx, .xlsx, .pptx) are supported.
const CONVERTIBLE = new Set([
	".pdf",
	".docx", ".xlsx", ".pptx",
	".html", ".htm",
]);

export function isConvertible(filePath: string): boolean {
	return CONVERTIBLE.has(extname(filePath).toLowerCase());
}

// ── Format converters ──────────────────────────────────────────────────

async function pdfToMarkdown(buffer: Buffer): Promise<string> {
	const parser = new PDFParse({ data: buffer });
	const result = await parser.getText();
	return result.text.trim();
}

async function docxToMarkdown(buffer: Buffer): Promise<string> {
	const result = await mammoth.convertToHtml({ buffer });
	return htmlToText(result.value);
}

async function xlsxToMarkdown(buffer: Buffer): Promise<string> {
	const workbook = XLSX.read(buffer, { type: "buffer" });
	const sections: string[] = [];

	for (const sheetName of workbook.SheetNames) {
		const sheet = workbook.Sheets[sheetName];
		const rows = XLSX.utils.sheet_to_json<unknown[]>(sheet, { header: 1, defval: "" });
		if (rows.length === 0) continue;

		const [header, ...body] = rows as string[][];
		const separator = header.map(() => "---");
		const table = [header, separator, ...body]
			.map((row) => "| " + row.map(String).join(" | ") + " |")
			.join("\n");

		sections.push(`## ${sheetName}\n\n${table}`);
	}

	return sections.join("\n\n");
}

async function pptxToMarkdown(buffer: Buffer): Promise<string> {
	const zip = await JSZip.loadAsync(buffer);
	const slideEntries = Object.keys(zip.files)
		.filter((name) => /^ppt\/slides\/slide\d+\.xml$/.test(name))
		.sort((a, b) => {
			const n = (s: string) => parseInt(s.match(/\d+/)![0], 10);
			return n(a) - n(b);
		});

	const slides: string[] = [];
	for (let i = 0; i < slideEntries.length; i++) {
		const xml = await zip.files[slideEntries[i]].async("string");
		const texts: string[] = [];
		for (const match of xml.matchAll(/<a:t[^>]*>([^<]*)<\/a:t>/g)) {
			const text = match[1].trim();
			if (text) texts.push(text);
		}
		if (texts.length > 0) {
			slides.push(`## Slide ${i + 1}\n\n${texts.join("\n\n")}`);
		}
	}

	return slides.join("\n\n---\n\n");
}

function htmlToText(html: string): string {
	return html
		.replace(/<style[^>]*>[\s\S]*?<\/style>/gi, "")
		.replace(/<script[^>]*>[\s\S]*?<\/script>/gi, "")
		.replace(/<br\s*\/?>/gi, "\n")
		.replace(/<\/p>/gi, "\n\n")
		.replace(/<\/h[1-6]>/gi, "\n\n")
		.replace(/<[^>]+>/g, "")
		.replace(/&nbsp;/g, " ")
		.replace(/&amp;/g, "&")
		.replace(/&lt;/g, "<")
		.replace(/&gt;/g, ">")
		.replace(/&quot;/g, '"')
		.replace(/\n{3,}/g, "\n\n")
		.trim();
}

// ── Post-processing (Stage 2a — headroom-style compression) ───────────

// Strips Table of Contents sections — they duplicate the doc outline we generate.
function stripTableOfContents(text: string): string {
	// Match a ToC block: heading containing "contents" / "index", followed by
	// lines that look like "Some Title .... 12" or "Some Title    12"
	return text.replace(
		/^(#{1,3}\s+(?:table\s+of\s+)?contents?|#{1,3}\s+index)\s*\n((?:[^\n]*?[\s.]{3,}\d+\s*\n?)*)/gim,
		"",
	).trim();
}

// Strips page number artifacts from PDF extraction:
//   "-- 1 of 86 --", "Page 1", "1 of 86", bare standalone "42"
//   "42\tsome text" — page-number prefix embedded at start of line
function stripPageArtifacts(text: string): string {
	return text
		.split("\n")
		.map((line) => line.replace(/^\d{1,3}\t/, "")) // strip leading "N\t" page prefix
		.filter((line) => {
			const t = line.trim();
			if (!t) return true;
			if (/^-+\s*\d+\s*(of\s+\d+)?\s*-+$/i.test(t)) return false; // -- 1 of 86 --
			if (/^page\s+\d+(\s+of\s+\d+)?$/i.test(t)) return false;     // Page 1 of 86
			if (/^\d+\s*(of\s+\d+)?$/.test(t)) return false;              // standalone number
			return true;
		})
		.join("\n");
}

// Strips decorative separator lines (lines made only of ─ = * - _).
function stripDecorativeLines(text: string): string {
	return text
		.split("\n")
		.filter((line) => !/^[\s\-=*_─═━■▪]+$/.test(line) || line.trim().length === 0)
		.join("\n");
}

// Collapses PDF alignment artifacts: multiple consecutive spaces → single space.
function normalizeWhitespace(text: string): string {
	return text
		.split("\n")
		.map((line) => line.replace(/  +/g, " ").trimEnd())
		.join("\n")
		.replace(/\n{3,}/g, "\n\n")
		.trim();
}

// Removes short lines that repeat 5+ times (page headers/footers).
// Keeps the first occurrence so the document still references the content.
function deduplicateBoilerplate(text: string): string {
	const lines = text.split("\n");
	const freq = new Map<string, number>();
	for (const line of lines) {
		const key = line.trim();
		if (key.length > 0 && key.length < 120) {
			freq.set(key, (freq.get(key) ?? 0) + 1);
		}
	}

	const seen = new Set<string>();
	return lines
		.filter((line) => {
			const key = line.trim();
			if (key.length === 0) return true;
			const count = freq.get(key) ?? 0;
			if (count >= 5) {
				if (seen.has(key)) return false;
				seen.add(key);
			}
			return true;
		})
		.join("\n")
		.replace(/\n{3,}/g, "\n\n")
		.trim();
}

// Stage 2a pipeline: apply all compression passes in order.
export function compressMarkdown(text: string): string {
	let out = text;
	out = stripTableOfContents(out);
	out = stripPageArtifacts(out);
	out = stripDecorativeLines(out);
	out = deduplicateBoilerplate(out);
	out = normalizeWhitespace(out);
	return out;
}

export type ConversionError = { error: string };

const MAX_FILE_SIZE_MB = 50;

// ── Main entry point ───────────────────────────────────────────────────

export async function convertToMarkdown(
	filePath: string,
	originalBuffer: Buffer,
): Promise<ConversionResult | ConversionError | null> {
	if (!isConvertible(filePath)) return null;
	if (process.env.DISABLE_DOC_CONVERSION === "1") return null;

	const fileSizeMB = originalBuffer.length / (1024 * 1024);
	if (fileSizeMB > MAX_FILE_SIZE_MB) {
		return { error: `File too large for conversion (${fileSizeMB.toFixed(1)} MB > ${MAX_FILE_SIZE_MB} MB limit). Convert manually and provide a text or markdown version.` };
	}

	let markdown = "";
	const ext = extname(filePath).toLowerCase();

	try {
		if (ext === ".pdf") {
			markdown = await pdfToMarkdown(originalBuffer);
		} else if (ext === ".docx" || ext === ".doc") {
			markdown = await docxToMarkdown(originalBuffer);
		} else if (ext === ".xlsx" || ext === ".xls") {
			markdown = await xlsxToMarkdown(originalBuffer);
		} else if (ext === ".pptx" || ext === ".ppt") {
			markdown = await pptxToMarkdown(originalBuffer);
		} else if (ext === ".html" || ext === ".htm") {
			markdown = htmlToText(originalBuffer.toString("utf-8"));
		}
	} catch (err) {
		return { error: `Conversion failed for ${basename(filePath)}: ${err instanceof Error ? err.message : String(err)}` };
	}

	if (!markdown.trim()) {
		return { error: `Conversion produced no readable text for ${basename(filePath)}. The file may be scanned/image-only or corrupted.` };
	}

	markdown = compressMarkdown(markdown);

	const originalTokens = Math.ceil(originalBuffer.length / 4);
	const convertedTokens = estimateTokens(markdown);
	const savedTokens = Math.max(0, originalTokens - convertedTokens);
	const savedPercent = originalTokens > 0
		? Math.round((savedTokens / originalTokens) * 100)
		: 0;

	return { markdown, originalTokens, convertedTokens, savedTokens, savedPercent };
}
