import { existsSync, readFileSync } from "fs";
import { join } from "path";
import yaml from "js-yaml";
import {
	git,
	gitOk,
	gitRaw,
	gitTry,
	gitWrite,
	hasCommits,
	isBareRepo,
	isGitRepo,
	isShallowRepo,
	refExists,
	repoPrefix,
	repoRoot,
} from "./git.js";

/** Tags live under refs/tags/agentcfg/ so agent versions never collide with a repo's release tags. */
export const TAG_NS = "agentcfg";

/**
 * What a version captures and what a rollback restores.
 *
 * memory/ and skills/ are deliberately absent: memory commits on every save
 * (src/tools/memory.ts) and skill_learner rewrites skills/ at runtime, so restoring
 * them would destroy knowledge the agent gained after the tag was cut.
 * Keep in sync with the files loadAgent() reads in src/loader.ts.
 */
export const CONFIG_PATHS: readonly string[] = [
	"agent.yaml",
	"SOUL.md",
	"RULES.md",
	"DUTIES.md",
	"AGENTS.md",
	"config",
	"tools",
	"hooks",
	"knowledge",
	"examples",
	"compliance",
	"agents",
	"workflows",
	"schedules",
	"plugins",
];

export type VersionErrorCode =
	| "NOT_A_REPO"
	| "BARE_REPO"
	| "NO_AGENT_YAML"
	| "NO_COMMITS"
	| "INVALID_NAME"
	| "TAG_EXISTS"
	| "UNKNOWN_VERSION"
	| "DIRTY_CONFIG"
	| "INDEX_DIRTY"
	| "DETACHED_HEAD"
	| "RESTORE_FAILED"
	| "COMMIT_FAILED";

export class VersionError extends Error {
	constructor(
		readonly code: VersionErrorCode,
		message: string,
		readonly paths: string[] = [],
	) {
		super(message);
		this.name = "VersionError";
	}
}

export interface VersionContext {
	agentDir: string;
	/** Repo root — every git command runs here so pathspecs are root-relative. */
	root: string;
	/** "" when the agent is the repo root, "bot/" when it lives in a subdirectory. */
	prefix: string;
	pathspecs: string[];
}

export interface VersionInfo {
	name: string;
	date: string;
	commit: string;
	subject: string;
	annotated: boolean;
}

export interface RollbackPlan {
	name: string;
	commit: string;
	/** Present in the tag, changed or gone now — write the tag's content back. */
	restore: string[];
	/** Absent from the tag, present now — added after the tag, so delete. */
	remove: string[];
}

export interface RollbackResult {
	changed: boolean;
	restored: number;
	removed: number;
	commit?: string;
}

export interface SaveResult {
	name: string;
	ref: string;
	commit: string;
	fileCount: number;
	committed: string[];
}

export interface ShowResult {
	name: string;
	commit: string;
	date: string;
	author: string;
	message: string;
	files: string[];
	driftStat: string;
}

export function tagRef(name: string): string {
	return `refs/tags/${TAG_NS}/${name}`;
}

// Stricter than git check-ref-format on purpose: forbidding "/" and whitespace is what
// makes the `for-each-ref --format=%(refname:strip=3)` parsing provably unambiguous.
const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

export function validateVersionName(name: string): void {
	const bad = (why: string) => {
		throw new VersionError("INVALID_NAME", `Invalid version name "${name}": ${why}`);
	};
	if (!name) bad("name is empty");
	if (name.length > 100) bad("name is longer than 100 characters");
	if (!NAME_RE.test(name)) {
		bad("use letters, digits, dot, dash and underscore only, starting with a letter or digit");
	}
	if (name.includes("..")) bad('name may not contain ".."');
	if (name.endsWith(".")) bad('name may not end with "."');
	if (name.endsWith(".lock")) bad('name may not end with ".lock"');
	if (name === "HEAD") bad('"HEAD" is reserved');
}

export function resolveContext(agentDir: string): VersionContext {
	if (!isGitRepo(agentDir)) {
		throw new VersionError(
			"NOT_A_REPO",
			`Not a git repository: ${agentDir}\nRun gitagent here once to initialize it, or pass --dir <path>.`,
		);
	}
	if (isBareRepo(agentDir)) {
		throw new VersionError("BARE_REPO", `Bare repository has no working tree: ${agentDir}`);
	}
	if (!existsSync(join(agentDir, "agent.yaml"))) {
		throw new VersionError("NO_AGENT_YAML", `No agent.yaml in ${agentDir} — not a gitagent agent.`);
	}
	if (!hasCommits(agentDir)) {
		throw new VersionError("NO_COMMITS", "Repository has no commits yet — nothing to version.");
	}
	const root = repoRoot(agentDir);
	const prefix = repoPrefix(agentDir);
	return { agentDir, root, prefix, pathspecs: CONFIG_PATHS.map((p) => prefix + p) };
}

export function isShallow(ctx: VersionContext): boolean {
	return isShallowRepo(ctx.root);
}

/** Split NUL-separated plumbing output, dropping the trailing empty field. */
function nulFields(raw: string): string[] {
	return raw.split("\0").filter((f) => f !== "");
}

/** Uncommitted config changes, including untracked files. Never reports memory/ or skills/. */
export function dirtyConfigPaths(ctx: VersionContext): string[] {
	const raw = gitRaw(ctx.root, ["status", "--porcelain", "-z", "--no-renames", "--", ...ctx.pathspecs]);
	// Each record is "XY <path>": two status chars plus a space.
	return nulFields(raw).map((rec) => rec.slice(3));
}

function assertIndexClean(ctx: VersionContext): void {
	if (!gitOk(ctx.root, ["diff", "--cached", "--quiet"])) {
		throw new VersionError(
			"INDEX_DIRTY",
			"The git index has staged changes. Commit or unstage them first —\n" +
				"otherwise they would be swept into the commit this command creates.",
		);
	}
}

function readManifestVersion(agentDir: string): string | null {
	try {
		const doc = yaml.load(readFileSync(join(agentDir, "agent.yaml"), "utf-8")) as any;
		const v = doc?.version;
		return typeof v === "string" && v.trim() ? v.trim() : null;
	} catch {
		return null;
	}
}

export function listVersions(ctx: VersionContext): VersionInfo[] {
	const format = [
		"%(refname:strip=3)",
		"%(objecttype)",
		"%(creatordate:iso-strict)",
		"%(objectname:short)",
		"%(*objectname:short)",
		"%(contents:subject)",
	].join("%09");
	const raw = git(ctx.root, [
		"for-each-ref",
		"--sort=-creatordate",
		`--format=${format}`,
		`refs/tags/${TAG_NS}/*`,
	]);
	if (!raw) return [];
	return raw.split("\n").map((line) => {
		const parts = line.split("\t");
		// Subject is last so it can safely contain tabs.
		const subject = parts.slice(5).join("\t");
		return {
			name: parts[0],
			annotated: parts[1] === "tag",
			date: parts[2] ?? "",
			// Annotated tags: prefer the dereferenced commit over the tag object.
			commit: parts[4] || parts[3] || "",
			subject,
		};
	});
}

function assertVersionExists(ctx: VersionContext, name: string): void {
	if (refExists(ctx.root, tagRef(name))) return;
	const known = listVersions(ctx).map((v) => v.name);
	const hint = known.length
		? `Known versions: ${known.join(", ")}`
		: "No versions saved yet. Create one with: gitagent version save v1.0";
	throw new VersionError("UNKNOWN_VERSION", `Unknown version "${name}". ${hint}`);
}

export interface SaveOptions {
	name?: string;
	message?: string;
	commit?: boolean;
	force?: boolean;
}

export function saveVersion(ctx: VersionContext, opts: SaveOptions = {}): SaveResult {
	let name = opts.name;
	if (!name) {
		const manifestVersion = readManifestVersion(ctx.agentDir);
		if (!manifestVersion) {
			throw new VersionError(
				"INVALID_NAME",
				"No version name given and agent.yaml has no version field.\nUsage: gitagent version save <name>",
			);
		}
		name = `v${manifestVersion.replace(/^v/, "")}`;
	}
	validateVersionName(name);

	assertIndexClean(ctx);

	if (refExists(ctx.root, tagRef(name)) && !opts.force) {
		throw new VersionError(
			"TAG_EXISTS",
			`Version "${name}" already exists. Use --force to move it, or pick another name.`,
		);
	}

	const dirty = dirtyConfigPaths(ctx);
	let committed: string[] = [];
	if (dirty.length > 0) {
		if (!opts.commit) {
			throw new VersionError(
				"DIRTY_CONFIG",
				`Uncommitted config changes would not be captured by "${name}".\n` +
					"Re-run with --commit to commit them first, or commit them yourself.",
				dirty,
			);
		}
		gitWrite(ctx.root, ["add", "--", ...dirty]);
		// Guard against a pathspec surprise sweeping in something outside the allowlist.
		const staged = gitRaw(ctx.root, ["diff", "--cached", "--name-only", "-z"]);
		const stagedPaths = nulFields(staged);
		const outside = stagedPaths.filter((p) => !dirty.includes(p));
		if (outside.length > 0) {
			throw new VersionError(
				"INDEX_DIRTY",
				`Refusing to commit: unexpected staged paths outside the config allowlist.`,
				outside,
			);
		}
		gitWrite(ctx.root, ["commit", "-m", `gitagent: snapshot config for ${name}`]);
		committed = stagedPaths;
	}

	const message = opts.message || `gitagent version ${name}`;
	const tagArgs = ["tag", "-a"];
	if (opts.force) tagArgs.push("-f");
	gitWrite(ctx.root, [...tagArgs, `${TAG_NS}/${name}`, "-m", message, "HEAD"]);

	const commit = git(ctx.root, ["rev-parse", "--short", "HEAD"]);
	const files = configFilesAt(ctx, name);
	return { name, ref: tagRef(name), commit, fileCount: files.length, committed };
}

export function configFilesAt(ctx: VersionContext, name: string): string[] {
	const raw = gitRaw(ctx.root, [
		"ls-tree",
		"-r",
		"--name-only",
		"-z",
		tagRef(name),
		"--",
		...ctx.pathspecs,
	]);
	return nulFields(raw);
}

export function showVersion(ctx: VersionContext, name: string): ShowResult {
	assertVersionExists(ctx, name);
	const ref = tagRef(name);
	const meta = git(ctx.root, ["show", "-s", "--format=%h%n%aI%n%an", `${ref}^{commit}`]).split("\n");
	const message = gitTry(ctx.root, ["for-each-ref", "--format=%(contents)", ref]) || "";
	return {
		name,
		commit: meta[0] ?? "",
		date: meta[1] ?? "",
		author: meta[2] ?? "",
		message: message.trim(),
		files: configFilesAt(ctx, name),
		driftStat: gitTry(ctx.root, ["diff", "--stat", ref, "HEAD", "--", ...ctx.pathspecs]) || "",
	};
}

/** Build the argv for a config-scoped diff. The pathspec suffix is what keeps memory/ out. */
export function buildDiffArgs(
	ctx: VersionContext,
	a: string,
	b: string | undefined,
	extra: string[] = [],
): string[] {
	assertVersionExists(ctx, a);
	if (b) assertVersionExists(ctx, b);
	const refs = b ? [tagRef(a), tagRef(b)] : [tagRef(a)];
	return ["diff", ...extra, ...refs, "--", ...ctx.pathspecs];
}

export function hasConfigDiff(ctx: VersionContext, a: string, b?: string): boolean {
	return !gitOk(ctx.root, buildDiffArgs(ctx, a, b, ["--quiet"]));
}

export function planRollback(ctx: VersionContext, name: string): RollbackPlan {
	assertVersionExists(ctx, name);
	const ref = tagRef(name);
	// --no-renames is mandatory: rename records are 3 fields and would desynchronize
	// the pairwise parse below. -z is mandatory so odd paths aren't C-quoted.
	const raw = gitRaw(ctx.root, [
		"diff",
		"--name-status",
		"--no-renames",
		"-z",
		ref,
		"HEAD",
		"--",
		...ctx.pathspecs,
	]);
	const fields = nulFields(raw);
	const restore: string[] = [];
	const remove: string[] = [];
	for (let i = 0; i + 1 < fields.length; i += 2) {
		const status = fields[i];
		const path = fields[i + 1];
		// Statuses are oriented tag -> HEAD.
		if (status.startsWith("A")) remove.push(path);
		else restore.push(path);
	}
	restore.sort();
	remove.sort();
	return { name, commit: git(ctx.root, ["rev-parse", "--short", `${ref}^{commit}`]), restore, remove };
}

export interface RollbackOptions {
	message?: string;
}

function chunk<T>(items: T[], size: number): T[][] {
	const out: T[][] = [];
	for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
	return out;
}

export function applyRollback(
	ctx: VersionContext,
	plan: RollbackPlan,
	opts: RollbackOptions = {},
): RollbackResult {
	if (plan.restore.length === 0 && plan.remove.length === 0) {
		return { changed: false, restored: 0, removed: 0 };
	}

	// Remove before restore: if a config path was a file at the tag and is a directory
	// now (or vice versa), checking out first fails with "Not a directory".
	//
	// A failure here leaves the plan half-applied and staged but NOT committed, which
	// is a different recovery story from a failed commit — hence the separate error.
	try {
		for (const paths of chunk(plan.remove, 500)) {
			gitWrite(ctx.root, ["rm", "-q", "-f", "--", ...paths]);
		}
		for (const paths of chunk(plan.restore, 500)) {
			gitWrite(ctx.root, ["checkout", tagRef(plan.name), "--", ...paths]);
		}
	} catch (err: any) {
		throw new VersionError(
			"RESTORE_FAILED",
			`Rollback failed partway through: ${err?.message ?? err}\n` +
				"Config files were partially modified and nothing was committed. " +
				"Run 'git status' to inspect, then 'git reset --hard HEAD' to recover " +
				"(note that also discards any other uncommitted changes, including memory/).",
		);
	}

	const subject = opts.message || `rollback config to ${plan.name}`;
	const body =
		`Restored ${plan.restore.length} file(s), removed ${plan.remove.length} file(s) ` +
		`from ${TAG_NS}/${plan.name} (${plan.commit}).`;
	try {
		// git rm and git checkout -- <paths> both stage their work, and the index was
		// verified clean beforehand, so a bare commit captures exactly this plan.
		gitWrite(ctx.root, ["commit", "-m", subject, "-m", body]);
	} catch (err: any) {
		throw new VersionError(
			"COMMIT_FAILED",
			`Config was fully restored but the commit failed: ${err?.message ?? err}\n` +
				"The complete rollback is staged. Run 'git commit' to finish it, or " +
				"'git reset --hard HEAD' to discard it (that also discards any other " +
				"uncommitted changes, including memory/).",
		);
	}

	return {
		changed: true,
		restored: plan.restore.length,
		removed: plan.remove.length,
		commit: git(ctx.root, ["rev-parse", "--short", "HEAD"]),
	};
}

/** Preconditions shared by rollback before a plan is built. */
export function assertRollbackReady(ctx: VersionContext, force: boolean): void {
	assertIndexClean(ctx);
	if (!force) {
		const dirty = dirtyConfigPaths(ctx);
		if (dirty.length > 0) {
			throw new VersionError(
				"DIRTY_CONFIG",
				"Uncommitted config changes would be overwritten by this rollback.\n" +
					"Commit them, or re-run with --force to discard them.",
				dirty,
			);
		}
	}
	if (!gitOk(ctx.root, ["symbolic-ref", "-q", "HEAD"]) && !force) {
		throw new VersionError(
			"DETACHED_HEAD",
			"HEAD is detached — the rollback commit would not land on any branch.\n" +
				"Check out a branch first, or re-run with --force.",
		);
	}
}
