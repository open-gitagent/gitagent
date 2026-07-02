import { readFile, readdir, stat } from "fs/promises";
import { join } from "path";
import { spawn } from "child_process";
import yaml from "js-yaml";
import { Type } from "@sinclair/typebox";
import type { AgentTool } from "@mariozechner/pi-agent-core";

interface ToolDefinition {
	name: string;
	description: string;
	input_schema: Record<string, any>;
	output_schema?: Record<string, any>;
	implementation: {
		script: string;
		runtime?: string;
	};
}

export function buildTypeboxSchema(schema: Record<string, any>): any {
	// Convert a simplified JSON-schema-like object to Typebox properties
	const properties: Record<string, any> = {};
	if (schema.properties) {
		for (const [key, def] of Object.entries(schema.properties) as [string, any][]) {
			const desc = def.description || "";
			const required = schema.required?.includes(key) ?? false;
			let prop;
			switch (def.type) {
				case "number":
					prop = Type.Number({ description: desc });
					break;
				case "boolean":
					prop = Type.Boolean({ description: desc });
					break;
				case "array":
					prop = Type.Array(Type.Any(), { description: desc });
					break;
				case "object":
					prop = Type.Any({ description: desc });
					break;
				default:
					prop = Type.String({ description: desc });
					break;
			}
			properties[key] = required ? prop : Type.Optional(prop);
		}
	}
	return Type.Object(properties);
}

function createDeclarativeTool(
	def: ToolDefinition,
	agentDir: string,
): AgentTool<any> {
	const schema = buildTypeboxSchema(def.input_schema);
	const scriptPath = join(agentDir, "tools", def.implementation.script);
	const runtime = def.implementation.runtime || "sh";

	return {
		name: def.name,
		label: def.name,
		description: def.description,
		parameters: schema,
		execute: async (
			_toolCallId: string,
			args: any,
			signal?: AbortSignal,
		) => {
			if (signal?.aborted) throw new Error("Operation aborted");

			return new Promise((resolve, reject) => {
				const child = spawn(runtime, [scriptPath], {
					cwd: agentDir,
					stdio: ["pipe", "pipe", "pipe"],
					env: { ...process.env },
				});

				let stdout = "";
				let stderr = "";

				child.stdout.on("data", (data: Buffer) => {
					stdout += data.toString("utf-8");
				});
				child.stderr.on("data", (data: Buffer) => {
					stderr += data.toString("utf-8");
				});

				// Send args as JSON on stdin
				child.stdin.write(JSON.stringify(args));
				child.stdin.end();

				const timeout = setTimeout(() => {
					child.kill("SIGTERM");
					reject(new Error(`Tool "${def.name}" timed out after 120s`));
				}, 120_000);

				const onAbort = () => child.kill("SIGTERM");
				if (signal) signal.addEventListener("abort", onAbort, { once: true });

				child.on("error", (err) => {
					clearTimeout(timeout);
					if (signal) signal.removeEventListener("abort", onAbort);
					reject(new Error(`Tool "${def.name}" failed to start: ${err.message}`));
				});

				child.on("close", (code) => {
					clearTimeout(timeout);
					if (signal) signal.removeEventListener("abort", onAbort);

					if (signal?.aborted) {
						reject(new Error("Operation aborted"));
						return;
					}

					if (code !== 0 && code !== null) {
						reject(new Error(`Tool "${def.name}" exited with code ${code}: ${stderr.trim()}`));
						return;
					}

					// Try parsing JSON output
					let text = stdout.trim();

					// Detect data URI — works regardless of field naming in user scripts.
					// Scan lines so stray log output before the URI doesn't break detection.
					const dataUriLine = text.split("\n").find(
						line => line.startsWith("data:image/") && line.includes(";base64,"),
					);
					if (dataUriLine) {
						const commaIndex = dataUriLine.indexOf(",");
						// Slice past the leading "data:" prefix (5 chars) to get "image/jpeg;base64"
						const mimeType = dataUriLine.slice(5, commaIndex).split(";")[0];
						const data = dataUriLine.slice(commaIndex + 1).replace(/\s/g, "");
						const allowedMimeTypes = ["image/jpeg", "image/png", "image/gif", "image/webp"];
						if (data && allowedMimeTypes.includes(mimeType)) {
							resolve({
								content: [{ type: "image", data, mimeType }],
								details: undefined,
							});
							return;
						}
						// Fall through to text/JSON handling if data is empty or mime type unsupported
					}

					try {
						const parsed = JSON.parse(text);
						if (parsed.text) text = parsed.text;
						else if (parsed.result) text = typeof parsed.result === "string" ? parsed.result : JSON.stringify(parsed.result);
					} catch {
						// Raw text output is fine
					}

					resolve({
						content: [{ type: "text", text: text || "(no output)" }],
						details: undefined,
					});
				});
			});
		},
	};
}

export async function loadDeclarativeTools(agentDir: string): Promise<AgentTool<any>[]> {
	const toolsDir = join(agentDir, "tools");

	try {
		const s = await stat(toolsDir);
		if (!s.isDirectory()) return [];
	} catch {
		return [];
	}

	const entries = await readdir(toolsDir);
	const tools: AgentTool<any>[] = [];

	for (const entry of entries) {
		if (!entry.endsWith(".yaml") && !entry.endsWith(".yml")) continue;

		try {
			const raw = await readFile(join(toolsDir, entry), "utf-8");
			const def = yaml.load(raw) as ToolDefinition;
			if (def?.name && def?.description && def?.input_schema && def?.implementation?.script) {
				tools.push(createDeclarativeTool(def, agentDir));
			}
		} catch {
			// Skip invalid tool definitions
		}
	}

	return tools;
}
