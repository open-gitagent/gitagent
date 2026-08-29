import { createInterface, type Interface } from "readline";
import { spawnSync } from "child_process";
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

// ANSI helpers (kept local, same style as index.ts)
const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string) => `\x1b[1m${s}\x1b[0m`;
const yellow = (s: string) => `\x1b[33m${s}\x1b[0m`;

// ── Types ───────────────────────────────────────────────────────────────

export interface ElicitChoice {
	/** Single character the user types to pick this option. */
	key: string;
	/** Short imperative label, e.g. `accept — write the file and commit`. */
	label: string;
}

export interface ElicitSelectRequest {
	title: string;
	/** Pre-rendered detail block (diff, stats, failure lessons) printed above the choices. */
	body?: string;
	choices: ElicitChoice[];
	/** Key used when the user just presses Enter, and when nobody can answer. */
	defaultKey: string;
	signal?: AbortSignal;
}

/**
 * A human-in-the-loop channel a tool can use mid-execution: it blocks the
 * agent loop until the user picks an option, so the *user's* decision — not
 * the model's guess — drives what happens next.
 *
 * `interactive` is false when nobody can answer (no TTY, or the approval
 * policy is "auto"). Callers MUST check it and keep their previous autonomous
 * behaviour in that case, so headless runs and CI don't hang or silently
 * change meaning.
 */
export interface Elicitor {
	readonly interactive: boolean;
	/** Returns the chosen `key`. Rejects with "Operation aborted" if `signal` fires. */
	select(req: ElicitSelectRequest): Promise<string>;
	/**
	 * Opens `initial` in $VISUAL/$EDITOR for hand-editing.
	 * Returns the edited text, or null if no editor was available or nothing changed.
	 */
	edit(initial: string, opts?: { extension?: string }): Promise<string | null>;
}

// ── Console implementation ──────────────────────────────────────────────

function firstWord(label: string): string {
	return label.split(/[\s—-]/)[0].toLowerCase();
}

function askOn(rl: Interface, query: string, signal?: AbortSignal): Promise<string> {
	return new Promise<string>((resolve, reject) => {
		if (signal?.aborted) {
			reject(new Error("Operation aborted"));
			return;
		}
		const onAbort = () => reject(new Error("Operation aborted"));
		signal?.addEventListener("abort", onAbort, { once: true });
		rl.question(query, { signal }, (answer) => {
			signal?.removeEventListener("abort", onAbort);
			resolve(answer);
		});
	});
}

export class ConsoleElicitor implements Elicitor {
	private rl: Interface | null = null;

	/**
	 * Share the REPL's readline interface. Required in REPL mode — two
	 * interfaces on the same stdin fight over input. In single-shot mode
	 * there is no REPL, so a short-lived interface is created per prompt.
	 */
	attach(rl: Interface | null): void {
		this.rl = rl;
	}

	get interactive(): boolean {
		// Escape hatch for scripted/CI runs that want today's fully autonomous behaviour.
		if (process.env.GITAGENT_APPROVAL === "auto") return false;
		return process.stdin.isTTY === true;
	}

	async select(req: ElicitSelectRequest): Promise<string> {
		if (!this.interactive) return req.defaultKey;

		const keys = req.choices.map((c) => c.key).join("/");
		let out = `\n${yellow(req.title)}\n`;
		if (req.body) out += `${req.body}\n`;
		out += "\n";
		for (const c of req.choices) {
			const marker = c.key === req.defaultKey ? dim(" (default)") : "";
			out += `  ${bold(`[${c.key}]`)} ${c.label}${marker}\n`;
		}
		process.stdout.write(out);

		for (;;) {
			const answer = (await this.question(`→ choose [${keys}]: `, req.signal)).trim().toLowerCase();
			if (!answer) return req.defaultKey;
			const hit = req.choices.find(
				(c) => c.key === answer || firstWord(c.label).startsWith(answer),
			);
			if (hit) return hit.key;
			process.stdout.write(dim(`  "${answer}" is not one of ${keys} — try again.\n`));
		}
	}

	async edit(initial: string, opts?: { extension?: string }): Promise<string | null> {
		if (!this.interactive) return null;

		const editorCmd = process.env.VISUAL || process.env.EDITOR ||
			(process.platform === "win32" ? "notepad" : "vi");
		const [cmd, ...cmdArgs] = editorCmd.split(/\s+/);

		const tmp = mkdtempSync(join(tmpdir(), "gitagent-edit-"));
		const file = join(tmp, `proposal${opts?.extension ?? ".txt"}`);
		try {
			writeFileSync(file, initial, "utf-8");

			// The child takes over the tty; pause the REPL's readline so it
			// doesn't consume the editor's keystrokes.
			this.rl?.pause();
			const res = spawnSync(cmd, [...cmdArgs, file], { stdio: "inherit" });
			this.rl?.resume();

			if (res.error || res.status !== 0) {
				const why = res.error?.message ?? `exited ${res.status}`;
				process.stdout.write(dim(`  Could not run "${editorCmd}" (${why}) — proposal left unchanged.\n`));
				return null;
			}

			const edited = readFileSync(file, "utf-8");
			if (edited.trim() === initial.trim()) {
				process.stdout.write(dim("  No changes saved.\n"));
				return null;
			}
			// An empty buffer is a mistake, not an edit — callers would otherwise
			// write out whatever "nothing" means for them.
			if (edited.trim().length === 0) {
				process.stdout.write(dim("  Saved file was empty — proposal left unchanged.\n"));
				return null;
			}
			return edited;
		} finally {
			try {
				rmSync(tmp, { recursive: true, force: true });
			} catch {
				// best-effort temp cleanup
			}
		}
	}

	private question(query: string, signal?: AbortSignal): Promise<string> {
		const shared = this.rl;
		if (shared) return askOn(shared, query, signal);

		const rl = createInterface({ input: process.stdin, output: process.stdout });
		return askOn(rl, query, signal).finally(() => rl.close());
	}
}

export function createConsoleElicitor(): ConsoleElicitor {
	return new ConsoleElicitor();
}
