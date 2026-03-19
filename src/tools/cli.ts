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

				const isWin = process.platform === "win32";
				const child = spawn(isWin ? "cmd" : "sh", isWin ? ["/c", command] : ["-c", command], {
					cwd,
					stdio: ["ignore", "pipe", "pipe"],
					env: { ...process.env },
				});

				let output = "";
				let timedOut = false;

				const timeoutHandle = setTimeout(() => {
					timedOut = true;
					child.kill("SIGTERM");
				}, timeoutSecs * 1000);

				const onData = (data: Buffer) => {
					output += data.toString("utf-8");

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
					child.kill("SIGTERM");
				};

				if (signal) {
					signal.addEventListener("abort", onAbort, { once: true });
				}

				child.on("error", (err) => {
					clearTimeout(timeoutHandle);
					if (signal) signal.removeEventListener("abort", onAbort);
					reject(err);
				});

				child.on("close", (code) => {
					clearTimeout(timeoutHandle);
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

					if (code !== 0 && code !== null) {
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
