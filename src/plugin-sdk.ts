import type { GCToolDefinition } from "./sdk-types.js";
import type { HookDefinition, HookResult } from "./hooks.js";
import type { MemoryLayerDef } from "./plugin-types.js";

// ── Plugin API (passed to register() functions) ────────────────────────

type HookEvent = "on_session_start" | "pre_tool_use" | "post_response" | "on_error";
type HookHandler = (ctx: Record<string, any>) => Promise<HookResult> | HookResult;

export interface GitclawPluginApi {
	/** Plugin identifier */
	pluginId: string;
	/** Plugin directory path */
	pluginDir: string;
	/** Resolved plugin config values */
	config: Record<string, any>;

	/** Register a tool the agent can use */
	registerTool(def: GCToolDefinition): void;

	/** Register a lifecycle hook */
	registerHook(event: HookEvent, handler: HookHandler): void;

	/** Append text to the system prompt */
	addPrompt(text: string): void;

	/** Register a memory layer the agent can use */
	registerMemoryLayer(layer: { name: string; path: string; description: string }): void;

	/** Logger for plugin output */
	logger: {
		info(msg: string): void;
		warn(msg: string): void;
		error(msg: string): void;
	};
}

// ── Internal API implementation ────────────────────────────────────────

interface InternalPluginApi extends GitclawPluginApi {
	getTools(): GCToolDefinition[];
	getHooks(): Record<HookEvent, HookDefinition[]> | null;
	getPrompt(): string;
	getMemoryLayers(): MemoryLayerDef[];
}

export function createPluginApi(
	pluginId: string,
	pluginDir: string,
	config: Record<string, any>,
): InternalPluginApi {
	const tools: GCToolDefinition[] = [];
	const hooks: Partial<Record<HookEvent, HookHandler[]>> = {};
	const memoryLayers: MemoryLayerDef[] = [];
	let promptText = "";

	const prefix = `[plugin:${pluginId}]`;

	return {
		pluginId,
		pluginDir,
		config,

		registerTool(def: GCToolDefinition) {
			tools.push(def);
		},

		registerHook(event: HookEvent, handler: HookHandler) {
			if (!hooks[event]) hooks[event] = [];
			hooks[event]!.push(handler);
		},

		addPrompt(text: string) {
			promptText = promptText ? `${promptText}\n\n${text}` : text;
		},

		registerMemoryLayer(layer: { name: string; path: string; description: string }) {
			memoryLayers.push(layer);
		},

		logger: {
			info(msg: string) { console.log(`${prefix} ${msg}`); },
			warn(msg: string) { console.warn(`${prefix} ${msg}`); },
			error(msg: string) { console.error(`${prefix} ${msg}`); },
		},

		getTools() {
			return tools;
		},

		getHooks() {
			const events = Object.keys(hooks) as HookEvent[];
			if (events.length === 0) return null;

			const result: Record<string, HookDefinition[]> = {};
			for (const event of events) {
				const handlers = hooks[event]!;
				// Wrap each programmatic handler as a HookDefinition
				// that uses a synthetic script path with inline execution
				result[event] = handlers.map((handler, i) => ({
					script: `__programmatic_${pluginId}_${event}_${i}`,
					description: `Programmatic hook from plugin ${pluginId}`,
					baseDir: pluginDir,
					_handler: handler, // attached for programmatic execution
				} as HookDefinition & { _handler: HookHandler }));
			}
			return result as Record<HookEvent, HookDefinition[]>;
		},

		getPrompt() {
			return promptText;
		},

		getMemoryLayers() {
			return memoryLayers;
		},
	};
}
