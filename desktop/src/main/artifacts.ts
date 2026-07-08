import { extname, basename, resolve, relative, isAbsolute, sep } from "path";
import { readFileSync } from "fs";
import { shell } from "electron";
import type { Artifact, ArtifactContent, ArtifactKind } from "../shared/types";

const CODE_EXT = new Set([
	".ts", ".tsx", ".js", ".jsx", ".py", ".rb", ".go", ".rs", ".java",
	".c", ".cpp", ".h", ".css", ".scss", ".json", ".yaml", ".yml",
	".sh", ".sql", ".toml", ".xml",
]);
const IMG_EXT = new Set([".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".bmp"]);

export function kindFor(path: string): ArtifactKind {
	const e = extname(path).toLowerCase();
	if (e === ".md" || e === ".markdown") return "md";
	if (e === ".html" || e === ".htm") return "html";
	if (e === ".pdf") return "pdf";
	if (IMG_EXT.has(e)) return "image";
	if (CODE_EXT.has(e)) return "code";
	return "other";
}

function mimeFor(kind: ArtifactKind, path: string): string {
	const e = extname(path).toLowerCase().slice(1);
	if (kind === "image") return e === "svg" ? "image/svg+xml" : `image/${e === "jpg" ? "jpeg" : e}`;
	if (kind === "pdf") return "application/pdf";
	if (kind === "html") return "text/html";
	return "text/plain";
}

// per-session: relative path → last-seen timestamp
const perSession = new Map<string, Map<string, number>>();

export function record(id: string, relPath: string): void {
	let m = perSession.get(id);
	if (!m) {
		m = new Map();
		perSession.set(id, m);
	}
	m.set(relPath, Date.now());
}

function isNoise(p: string): boolean {
	return p.startsWith(".gitagent/") || p.startsWith("memory/") || p === "agent.yaml" || p === ".gitignore";
}

export function listArtifacts(id: string, _dir: string): Artifact[] {
	// In-memory only. We deliberately do NOT shell out to `git status` here — a
	// synchronous execSync on the main thread freezes the UI on large repos.
	// write/edit tool calls are tracked via record(); cli-written files can be
	// surfaced out-of-band later.
	const m = perSession.get(id) ?? new Map<string, number>();
	return [...m.entries()]
		.filter(([p]) => !isNoise(p))
		.sort((a, b) => b[1] - a[1])
		.map(([path, updatedAt]) => ({ path, name: basename(path), kind: kindFor(path), updatedAt }));
}

function jailed(dir: string, relPath: string): string {
	const abs = resolve(dir, relPath);
	const rel = relative(resolve(dir), abs);
	if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
		throw new Error("artifact path escapes the session directory");
	}
	return abs;
}

export function readArtifact(dir: string, relPath: string): ArtifactContent {
	const abs = jailed(dir, relPath);
	const kind = kindFor(relPath);
	const mime = mimeFor(kind, relPath);
	const name = basename(relPath);
	if (kind === "image" || kind === "pdf") {
		return { path: relPath, name, kind, mime, base64: readFileSync(abs).toString("base64") };
	}
	return { path: relPath, name, kind, mime, text: readFileSync(abs, "utf-8") };
}

export function openArtifact(dir: string, relPath: string): void {
	void shell.openPath(jailed(dir, relPath));
}
