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

/**
 * Recursively convert a JSON-Schema node into a TypeBox schema. Handles the
 * full set of types tool authors and MCP servers use: string/number/integer/
 * boolean/null, enums (→ union of literals), typed arrays (real item type),
 * nested objects (real properties + required), and union `type` arrays. Unknown
 * or missing types degrade to `Type.Any()` so the call still goes through and
 * the server validates the actual payload.
 */
function jsonSchemaToTypebox(def: any): any {
	if (!def || typeof def !== "object") return Type.Any();
	const desc = def.description || "";
	const opts = desc ? { description: desc } : {};

	// enum → union of literals (preserves the allowed values for the model)
	if (Array.isArray(def.enum) && def.enum.length > 0) {
		if (def.enum.length === 1) return Type.Literal(def.enum[0], opts);
		return Type.Union(def.enum.map((v: any) => Type.Literal(v)), opts);
	}

	// union type, e.g. ["string", "null"]
	if (Array.isArray(def.type)) {
		const variants = def.type.map((t: string) => jsonSchemaToTypebox({ ...def, type: t, description: undefined }));
		return variants.length === 1 ? variants[0] : Type.Union(variants, opts);
	}

	switch (def.type) {
		case "string":
			return Type.String(opts);
		case "number":
			return Type.Number(opts);
		case "integer":
			return Type.Integer(opts);
		case "boolean":
			return Type.Boolean(opts);
		case "null":
			return Type.Null(opts);
		case "array":
			return Type.Array(def.items ? jsonSchemaToTypebox(def.items) : Type.Any(), opts);
		case "object":
			return buildTypeboxSchema(def, opts);
		default:
			// No/unknown type — fall back to permissive Any.
			return Type.Any(opts);
	}
}

/**
 * Build a TypeBox object schema from a JSON-Schema-like object (with
 * `properties` and `required`). Used for both declarative YAML tools and MCP
 * tool input schemas.
 */
export function buildTypeboxSchema(schema: Record<string, any>, opts: Record<string, any> = {}): any {
	const properties: Record<string, any> = {};
	if (schema.properties) {
		for (const [key, def] of Object.entries(schema.properties) as [string, any][]) {
			const required = schema.required?.includes(key) ?? false;
			const prop = jsonSchemaToTypebox(def);
			properties[key] = required ? prop : Type.Optional(prop);
		}
	}
	return Type.Object(properties, opts);
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
