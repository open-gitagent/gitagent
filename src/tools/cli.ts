import { spawn } from "child_process";
import type { AgentTool, AgentToolUpdateCallback } from "@mariozechner/pi-agent-core";
import { cliSchema, MAX_OUTPUT, DEFAULT_TIMEOUT } from "./shared.js";

export function createCliTool(cwd: string, defaultTimeout?: number): AgentTool<typeof cliSchema> {
	const baseTimeout = defaultTimeout ?? DEFAULT_TIMEOUT;
	return {
		name: "cli",
		label: "cli",
		description:
			"Execute a shell command. Returns stdout and stderr combined. Output is truncated if it exceeds ~100KB. Default timeout is 120 seconds.",
		parameters: cliSchema,
		execute: async (
			_toolCallId: string,
			{ command, timeout }: { command: string; timeout?: number },
			signal?: AbortSignal,
			onUpdate?: AgentToolUpdateCallback,
		) => {
			const timeoutSecs = timeout ?? baseTimeout;

			return new Promise((resolve, reject) => {
				if (signal?.aborted) {
					reject(new Error("Operation aborted"));
					return;
				}

				// shell: true routes through cmd.exe on Windows and /bin/sh elsewhere.
				const isWin = process.platform === "win32";
				const child = spawn(command, {
					cwd,
					stdio: ["ignore", "pipe", "pipe"],
					env: { ...process.env },
					shell: true,
					// Own process group on POSIX so we can kill the whole tree. A bare
					// child.kill() hits only the shell, leaving grandchildren (e.g.
					// `foo &`) alive and holding the stdout pipe open — 'close' then
					// never fires and the tool hangs forever.
					detached: !isWin,
				});

				let output = "";
				let timedOut = false;
				let forceKill: ReturnType<typeof setTimeout> | undefined;

				const killTree = (sig: NodeJS.Signals) => {
					try {
						if (!isWin && child.pid) process.kill(-child.pid, sig);
						else child.kill(sig);
					} catch { /* already exited */ }
				};
				// SIGTERM the group, then SIGKILL it if it doesn't die within 3s.
				const terminate = () => {
					killTree("SIGTERM");
					forceKill = setTimeout(() => killTree("SIGKILL"), 3000);
				};

				const timeoutHandle = setTimeout(() => {
					timedOut = true;
					terminate();
				}, timeoutSecs * 1000);

				const onData = (data: Buffer) => {
					output += data.toString("utf-8");
					// Bound memory: keep a rolling tail rather than buffering GBs from
					// a runaway command (e.g. `yes`).
					if (output.length > MAX_OUTPUT * 2) output = output.slice(-MAX_OUTPUT);

					if (onUpdate && output.length <= MAX_OUTPUT) {
						onUpdate({
							content: [{ type: "text", text: output }],
							details: undefined,
						});
					}
				};

				child.stdout?.on("data", onData);
				child.stderr?.on("data", onData);

				const onAbort = () => {
					terminate();
				};

				if (signal) {
					signal.addEventListener("abort", onAbort, { once: true });
				}

				child.on("error", (err) => {
					clearTimeout(timeoutHandle);
					if (forceKill) clearTimeout(forceKill);
					if (signal) signal.removeEventListener("abort", onAbort);
					reject(err);
				});

				child.on("close", (code, killSignal) => {
					clearTimeout(timeoutHandle);
					if (forceKill) clearTimeout(forceKill);
					if (signal) signal.removeEventListener("abort", onAbort);

					if (signal?.aborted) {
						reject(new Error("Operation aborted"));
						return;
					}

					if (timedOut) {
						reject(new Error(`Command timed out after ${timeoutSecs} seconds\n${output}`));
						return;
					}

					// Truncate if needed
					let text = output;
					if (text.length > MAX_OUTPUT) {
						text = text.slice(-MAX_OUTPUT);
						text = `[output truncated, showing last ~100KB]\n${text}`;
					}

					if (!text) {
						text = "(no output)";
					}

					if (code === null) {
						// Killed by a signal (not our timeout/abort) — that's a failure.
						text += `\n\nKilled by signal: ${killSignal ?? "unknown"}`;
						reject(new Error(text));
					} else if (code !== 0) {
						text += `\n\nExit code: ${code}`;
						reject(new Error(text));
					} else {
						resolve({
							content: [{ type: "text", text }],
							details: { exitCode: code },
						});
					}
				});
			});
		},
	};
}
