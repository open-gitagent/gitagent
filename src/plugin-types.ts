import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { HooksConfig } from "./hooks.js";
import type { SkillMetadata } from "./skills.js";
import type { GCToolDefinition } from "./sdk-types.js";

// ── Plugin manifest (plugin.yaml) ─────────────────────────────────────

export interface PluginConfigProperty {
	type: "string" | "number" | "boolean";
	description?: string;
	default?: any;
	env?: string; // env var fallback
}

export interface PluginManifest {
	id: string;
	name: string;
	version: string;
	description: string;
	author?: string;
	license?: string;
	provides?: {
		tools?: boolean;
		hooks?: {
			on_session_start?: Array<{ script: string; description?: string }>;
			pre_tool_use?: Array<{ script: string; description?: string }>;
			post_response?: Array<{ script: string; description?: string }>;
			on_error?: Array<{ script: string; description?: string }>;
		};
		skills?: boolean;
		prompt?: string; // relative path to prompt file
	};
	config?: {
		properties?: Record<string, PluginConfigProperty>;
		required?: string[];
	};
	entry?: string; // optional programmatic entry point (e.g., index.ts)
	engine?: string; // min gitclaw version (e.g., ">=0.3.0")
}

// ── Plugin config in agent.yaml ────────────────────────────────────────

export interface PluginConfig {
	enabled?: boolean;
	source?: string; // git URL for remote plugins
	version?: string; // git branch/tag for remote plugins
	config?: Record<string, any>;
}

// ── Memory layer definition ─────────────────────────────────────────────

export interface MemoryLayerDef {
	name: string;
	path: string;
	description: string;
}

// ── Loaded plugin (resolved and ready to use) ──────────────────────────

export interface LoadedPlugin {
	manifest: PluginManifest;
	directory: string;
	config: Record<string, any>; // resolved config values
	tools: AgentTool<any>[]; // loaded declarative tools
	programmaticTools: GCToolDefinition[]; // tools from register()
	hooks: HooksConfig | null; // loaded hook definitions
	skills: SkillMetadata[]; // discovered skills
	promptAddition: string; // loaded prompt file content
	memoryLayers: MemoryLayerDef[]; // memory layers from register()
}
