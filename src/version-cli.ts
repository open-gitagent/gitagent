import { createInterface } from "readline";
import { createRequire } from "module";
import { gitStream } from "./git.js";
import {
	TAG_NS,
	VersionError,
	applyRollback,
	assertRollbackReady,
	buildDiffArgs,
	hasConfigDiff,
	isShallow,
	listVersions,
	planRollback,
	resolveContext,
	saveVersion,
	showVersion,
} from "./version.js";
import type { RollbackPlan, VersionContext } from "./version.js";

const require = createRequire(import.meta.url);
const { version: GITAGENT_VERSION } = require("../package.json");

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string) => `\x1b[1m${s}\x1b[0m`;
const red = (s: string) => `\x1b[31m${s}\x1b[0m`;
const green = (s: string) => `\x1b[32m${s}\x1b[0m`;

function flagValue(args: string[], ...names: string[]): string | undefined {
	for (const name of names) {
		const i = args.indexOf(name);
		if (i !== -1 && args[i + 1]) return args[i + 1];
	}
	return undefined;
}

/** Positional args: everything that isn't a flag or a flag's value. */
function positionals(args: string[], valued: string[]): string[] {
	const out: string[] = [];
	for (let i = 0; i < args.length; i++) {
		if (valued.includes(args[i])) {
			i++;
			continue;
		}
		if (args[i].startsWith("-")) continue;
		out.push(args[i]);
	}
	return out;
}

function confirm(question: string): Promise<boolean> {
	return new Promise((resolve) => {
		const rl = createInterface({ input: process.stdin, output: process.stdout });
		rl.question(`${question} [y/N] `, (answer) => {
			rl.close();
			resolve(/^y(es)?$/i.test(answer.trim()));
		});
	});
}

function printPaths(paths: string[], limit = 20): void {
	for (const p of paths.slice(0, limit)) console.log(`    ${p}`);
	if (paths.length > limit) console.log(dim(`    …and ${paths.length - limit} more`));
}

function warnIfShallow(ctx: VersionContext): void {
	if (isShallow(ctx)) {
		console.log(dim("Note: shallow clone — some tags may be missing. git fetch --unshallow --tags"));
	}
}

// ── Subcommands ────────────────────────────────────────────────────────

async function handleSave(agentDir: string, args: string[]): Promise<void> {
	const ctx = resolveContext(agentDir);
	const name = positionals(args, ["-m", "--message"])[0];
	const result = saveVersion(ctx, {
		name,
		message: flagValue(args, "-m", "--message"),
		commit: args.includes("--commit"),
		force: args.includes("--force"),
	});

	if (result.committed.length > 0) {
		console.log(dim(`Committed ${result.committed.length} config file(s) first.`));
	}
	console.log(`${green("Saved version")} ${bold(result.name)} ${dim(`(${result.commit})`)}`);
	console.log(dim(`${result.fileCount} config files tracked · memory/ and skills/ not included`));
	console.log(dim(`Local only — push with: git push origin ${result.ref}`));
}

function handleList(agentDir: string, args: string[]): void {
	const ctx = resolveContext(agentDir);
	const versions = listVersions(ctx);

	if (args.includes("--json")) {
		console.log(JSON.stringify(versions, null, 2));
		return;
	}
	if (versions.length === 0) {
		console.log(dim("No versions saved yet. Create one with: gitagent version save v1.0"));
		return;
	}
	const width = Math.max(...versions.map((v) => v.name.length));
	for (const v of versions) {
		const meta = dim(`${v.date.slice(0, 10)}  ${v.commit}`);
		console.log(`  ${bold(v.name.padEnd(width))}  ${meta}  ${v.subject}`);
	}
	warnIfShallow(ctx);
}

function handleShow(agentDir: string, args: string[]): void {
	const ctx = resolveContext(agentDir);
	const name = positionals(args, [])[0];
	if (!name) {
		console.error(red("Usage: gitagent version show <name> [--files]"));
		process.exit(1);
	}
	const info = showVersion(ctx, name);

	console.log(`${bold(info.name)} ${dim(`(${info.commit})`)}`);
	if (info.message) console.log(info.message);
	console.log(dim(`${info.author} · ${info.date}`));
	console.log();
	console.log(dim(`${info.files.length} config file(s) at this version:`));
	printPaths(info.files, args.includes("--files") ? info.files.length : 20);
	console.log();
	if (info.driftStat) {
		console.log(dim("Changes since this version:"));
		console.log(info.driftStat);
		console.log(dim(`Full patch: gitagent version diff ${info.name}`));
	} else {
		console.log(dim("Current config matches this version."));
	}
	warnIfShallow(ctx);
}

function handleDiff(agentDir: string, args: string[]): void {
	const ctx = resolveContext(agentDir);
	const [a, b] = positionals(args, []);
	if (!a) {
		console.error(red("Usage: gitagent version diff <a> [<b>] [--stat] [--name-only]"));
		process.exit(1);
	}
	const extra: string[] = [];
	if (args.includes("--stat")) extra.push("--stat");
	if (args.includes("--name-only")) extra.push("--name-only");

	if (!hasConfigDiff(ctx, a, b)) {
		console.log(dim("No config differences."));
		return;
	}
	console.log(dim(`${a} → ${b ?? "working tree"}`));
	// Exit status is intentionally ignored: git diff exits 1 when differences exist,
	// and hasConfigDiff above already established that there are some.
	gitStream(ctx.root, buildDiffArgs(ctx, a, b, extra));
}

function printPlan(plan: RollbackPlan): void {
	if (plan.restore.length > 0) {
		console.log(dim(`Restore ${plan.restore.length} file(s):`));
		printPaths(plan.restore);
	}
	if (plan.remove.length > 0) {
		console.log(dim(`Remove ${plan.remove.length} file(s) added after the version:`));
		printPaths(plan.remove);
	}
}

async function handleRollback(agentDir: string, args: string[]): Promise<void> {
	const ctx = resolveContext(agentDir);
	const name = positionals(args, ["-m", "--message"])[0];
	if (!name) {
		console.error(red("Usage: gitagent version rollback <name> [--dry-run] [--yes] [--force]"));
		process.exit(1);
	}
	const force = args.includes("--force");
	assertRollbackReady(ctx, force);

	const plan = planRollback(ctx, name);
	if (plan.restore.length === 0 && plan.remove.length === 0) {
		console.log(dim(`Already at ${name} — no config changes to apply.`));
		return;
	}

	printPlan(plan);
	if (args.includes("--dry-run")) {
		console.log(dim("Dry run — nothing changed."));
		return;
	}

	if (!args.includes("--yes")) {
		if (!process.stdin.isTTY) {
			console.error(red("Refusing to roll back without confirmation. Pass --yes to run non-interactively."));
			process.exit(1);
		}
		if (!(await confirm(`Roll back config to ${bold(name)}?`))) {
			console.log(dim("Aborted."));
			return;
		}
	}

	// force is consumed by assertRollbackReady above; applyRollback has no use for it.
	const result = applyRollback(ctx, plan, { message: flagValue(args, "-m", "--message") });
	console.log(`${green("Rolled back config to")} ${bold(name)}`);
	console.log(
		dim(`restored ${result.restored} file(s) · removed ${result.removed} file(s) · new commit ${result.commit}`),
	);
	console.log(dim("History preserved — this is a forward commit; memory/ and skills/ untouched."));
}

function printHelp(): void {
	console.log(`gitagent ${GITAGENT_VERSION}\n`);
	console.log(bold("gitagent version") + " — agent config versioning\n");
	console.log("Commands:");
	console.log(`  ${bold("save")} [<name>]        Tag the current config as a version`);
	console.log(`  ${bold("list")}                 List saved versions`);
	console.log(`  ${bold("show")} <name>          Show a version and what changed since`);
	console.log(`  ${bold("diff")} <a> [<b>]       Diff a version against another, or the working tree`);
	console.log(`  ${bold("rollback")} <name>      Restore config from a version as a new commit`);
	console.log();
	console.log(dim("Rollback restores config only — memory/ and skills/ are never modified."));
	console.log(dim("Versions are git tags under " + TAG_NS + "/ and stay local until you push them."));
}

// ── Main CLI handler ───────────────────────────────────────────────────

export async function handleVersionCommand(agentDir: string, args: string[]): Promise<void> {
	const subcommand = args[0];
	const subArgs = args.slice(1);

	try {
		switch (subcommand) {
			case "save":
			case "tag":
				await handleSave(agentDir, subArgs);
				break;
			case "list":
			case "ls":
				handleList(agentDir, subArgs);
				break;
			case "show":
				handleShow(agentDir, subArgs);
				break;
			case "diff":
				handleDiff(agentDir, subArgs);
				break;
			case "rollback":
			case "restore":
				await handleRollback(agentDir, subArgs);
				break;
			case undefined:
				printHelp();
				break;
			default:
				console.error(red(`Unknown version subcommand: "${subcommand}"`));
				console.error(dim(`If you meant a prompt, use: gitagent -p "version ${args.join(" ")}"`));
				console.error();
				printHelp();
				process.exit(1);
		}
	} catch (err) {
		if (err instanceof VersionError) {
			console.error(red(err.message));
			if (err.paths.length > 0) printPaths(err.paths);
			process.exit(1);
		}
		throw err;
	}
}

export function printToolVersion(): void {
	console.log(GITAGENT_VERSION);
}
