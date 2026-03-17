import { readFile, readdir, stat, mkdir, rm } from "fs/promises";
import { join } from "path";
import { execFileSync } from "child_process";
import { createRequire } from "module";
import { homedir } from "os";
import yaml from "js-yaml";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import type {
	PluginManifest,
	PluginConfig,
	LoadedPlugin,
	PluginConfigProperty,
} from "./plugin-types.js";
import type { HooksConfig, HookDefinition } from "./hooks.js";
import { loadDeclarativeTools } from "./tool-loader.js";
import { discoverSkills } from "./skills.js";
import type { SkillMetadata } from "./skills.js";

const require = createRequire(import.meta.url);
const { version: GITCLAW_VERSION } = require("../package.json");

const KEBAB_RE = /^[a-z0-9]+(-[a-z0-9]+)*$/;

// ── Engine version check ────────────────────────────────────────────────

function satisfiesEngine(range: string, current: string): boolean {
	const match = range.match(/^>=\s*(\d+\.\d+\.\d+)/);
	if (!match) return true; // Unknown format, allow
	const required = match[1].split(".").map(Number);
	const actual = current.split(".").map(Number);
	for (let i = 0; i < 3; i++) {
		if (actual[i] > required[i]) return true;
		if (actual[i] < required[i]) return false;
	}
	return true; // Equal
}

// ── Validation ─────────────────────────────────────────────────────────

function validatePluginManifest(manifest: any, pluginDir: string): manifest is PluginManifest {
	if (!manifest || typeof manifest !== "object") {
		console.warn(`Plugin at "${pluginDir}": invalid plugin.yaml`);
		return false;
	}
	if (!manifest.id || typeof manifest.id !== "string") {
		console.warn(`Plugin at "${pluginDir}": missing or invalid "id"`);
		return false;
	}
	if (!KEBAB_RE.test(manifest.id)) {
		console.warn(`Plugin "${manifest.id}": id must be kebab-case`);
		return false;
	}
	if (!manifest.name || !manifest.version || !manifest.description) {
		console.warn(`Plugin "${manifest.id}": missing name, version, or description`);
		return false;
	}
	return true;
}

// ── Config resolution ──────────────────────────────────────────────────

function resolvePluginConfig(
	manifest: PluginManifest,
	userConfig: Record<string, any> | undefined,
): Record<string, any> {
	const resolved: Record<string, any> = {};
	const schema = manifest.config;
	if (!schema?.properties) return resolved;

	for (const [key, prop] of Object.entries(schema.properties) as [string, PluginConfigProperty][]) {
		// Priority: user config > env var > default
		if (userConfig && userConfig[key] !== undefined) {
			let value = userConfig[key];
			// Resolve ${ENV_VAR} syntax
			if (typeof value === "string") {
				value = value.replace(/\$\{(\w+)\}/g, (_, envName) => process.env[envName] || "");
			}
			resolved[key] = value;
		} else if (prop.env && process.env[prop.env]) {
			resolved[key] = coerceValue(process.env[prop.env]!, prop.type);
		} else if (prop.default !== undefined) {
			resolved[key] = prop.default;
		}
	}

	// Check required fields
	if (schema.required) {
		for (const req of schema.required) {
			if (resolved[req] === undefined || resolved[req] === "") {
				console.warn(`Plugin "${manifest.id}": required config "${req}" is not set`);
			}
		}
	}

	return resolved;
}

function coerceValue(value: string, type: string): any {
	switch (type) {
		case "number": return Number(value);
		case "boolean": return value === "true" || value === "1";
		default: return value;
	}
}

// ── Directory helpers ──────────────────────────────────────────────────

async function dirExists(path: string): Promise<boolean> {
	try {
		const s = await stat(path);
		return s.isDirectory();
	} catch {
		return false;
	}
}

async function fileExists(path: string): Promise<boolean> {
	try {
		await stat(path);
		return true;
	} catch {
		return false;
	}
}

// ── Plugin installation ────────────────────────────────────────────────

export async function installPlugin(
	source: string,
	targetDir: string,
	version?: string,
	force?: boolean,
): Promise<string> {
	await mkdir(targetDir, { recursive: true });

	// Derive plugin name from source
	const name = source.split("/").pop()?.replace(/\.git$/, "") || "plugin";
	const pluginDir = join(targetDir, name);

	if (await dirExists(pluginDir)) {
		// Verify it's a valid plugin directory
		if (await fileExists(join(pluginDir, "plugin.yaml"))) {
			if (force) {
				await rm(pluginDir, { recursive: true, force: true });
			} else {
				console.log(`Plugin "${name}" already installed. Use --force to reinstall.`);
				return pluginDir;
			}
		} else {
			// Stale directory: remove and re-clone
			await rm(pluginDir, { recursive: true, force: true });
		}
	}

	const args = ["clone", "--depth", "1"];
	if (version) {
		args.push("--branch", version);
	}
	args.push(source, pluginDir);
	try {
		execFileSync("git", args, { stdio: "pipe" });
	} catch (err: any) {
		throw new Error(`Failed to install plugin from "${source}": ${err.message}`);
	}

	return pluginDir;
}

// ── Load a single plugin ───────────────────────────────────────────────

async function loadPlugin(
	pluginDir: string,
	userConfig: PluginConfig | undefined,
): Promise<LoadedPlugin | null> {
	const manifestPath = join(pluginDir, "plugin.yaml");
	if (!(await fileExists(manifestPath))) return null;

	let raw: string;
	try {
		raw = await readFile(manifestPath, "utf-8");
	} catch {
		return null;
	}

	const manifest = yaml.load(raw) as any;
	if (!validatePluginManifest(manifest, pluginDir)) return null;

	// Check engine compatibility
	if (manifest.engine && !satisfiesEngine(manifest.engine, GITCLAW_VERSION)) {
		console.warn(`Plugin "${manifest.id}": requires engine ${manifest.engine}, current is ${GITCLAW_VERSION}`);
		return null;
	}

	// Resolve config
	const config = resolvePluginConfig(manifest, userConfig?.config);

	// Load declarative tools
	let tools: AgentTool<any>[] = [];
	if (manifest.provides?.tools) {
		tools = await loadDeclarativeTools(pluginDir);
	}

	// Load hooks
	let hooks: HooksConfig | null = null;
	if (manifest.provides?.hooks) {
		const hooksDef: HooksConfig["hooks"] = {};
		for (const event of ["on_session_start", "pre_tool_use", "post_response", "on_error"] as const) {
			const entries = manifest.provides.hooks[event];
			if (entries && Array.isArray(entries)) {
				hooksDef[event] = entries.map((e: any) => ({
					script: e.script,
					description: e.description,
					baseDir: pluginDir,
				} as HookDefinition));
			}
		}
		const hasAny = Object.values(hooksDef).some((arr) => arr && arr.length > 0);
		if (hasAny) hooks = { hooks: hooksDef };
	}

	// Discover skills
	let skills: SkillMetadata[] = [];
	if (manifest.provides?.skills) {
		skills = await discoverSkills(pluginDir);
	}

	// Load prompt addition
	let promptAddition = "";
	if (manifest.provides?.prompt) {
		try {
			promptAddition = await readFile(join(pluginDir, manifest.provides.prompt), "utf-8");
		} catch {
			// Prompt file not found, skip
		}
	}

	// Load programmatic entry point
	let programmaticTools: any[] = [];
	let memoryLayers: import("./plugin-types.js").MemoryLayerDef[] = [];
	if (manifest.entry) {
		try {
			const { createPluginApi } = await import("./plugin-sdk.js");
			const api = createPluginApi(manifest.id, pluginDir, config);
			const entryPath = join(pluginDir, manifest.entry);
			const mod = await import(entryPath);
			if (typeof mod.register === "function") {
				await mod.register(api);
			} else if (typeof mod.default === "function") {
				await mod.default(api);
			}
			programmaticTools = api.getTools();
			// Merge programmatic hooks
			const progHooks = api.getHooks();
			if (progHooks) {
				if (!hooks) hooks = { hooks: {} };
				for (const event of ["on_session_start", "pre_tool_use", "post_response", "on_error"] as const) {
					if (progHooks[event]) {
						hooks.hooks[event] = [...(hooks.hooks[event] || []), ...progHooks[event]!];
					}
				}
			}
			// Merge programmatic prompt
			const extraPrompt = api.getPrompt();
			if (extraPrompt) {
				promptAddition = promptAddition ? `${promptAddition}\n\n${extraPrompt}` : extraPrompt;
			}
			// Collect memory layers
			memoryLayers = api.getMemoryLayers();
		} catch (err: any) {
			console.warn(`Plugin "${manifest.id}": failed to load entry "${manifest.entry}": ${err.message}`);
		}
	}

	return {
		manifest,
		directory: pluginDir,
		config,
		tools,
		programmaticTools,
		hooks,
		skills,
		promptAddition,
		memoryLayers,
	};
}

// ── Plugin discovery ───────────────────────────────────────────────────

async function discoverPluginDirs(
	pluginName: string,
	agentDir: string,
	gitagentDir: string,
): Promise<string | null> {
	// 1. Local: <agent-dir>/plugins/<name>/
	const localDir = join(agentDir, "plugins", pluginName);
	if (await dirExists(localDir)) return localDir;

	// 2. Global: ~/.gitclaw/plugins/<name>/
	const globalDir = join(homedir(), ".gitclaw", "plugins", pluginName);
	if (await dirExists(globalDir)) return globalDir;

	// 3. Installed: <agent-dir>/.gitagent/plugins/<name>/
	const installedDir = join(gitagentDir, "plugins", pluginName);
	if (await dirExists(installedDir)) return installedDir;

	return null;
}

// ── Main entry point ───────────────────────────────────────────────────

export async function discoverAndLoadPlugins(
	agentDir: string,
	gitagentDir: string,
	pluginsConfig: Record<string, PluginConfig> | undefined,
): Promise<LoadedPlugin[]> {
	if (!pluginsConfig || Object.keys(pluginsConfig).length === 0) {
		return [];
	}

	const loaded: LoadedPlugin[] = [];
	const toolNames = new Set<string>();

	for (const [pluginName, pluginConf] of Object.entries(pluginsConfig)) {
		// Skip disabled plugins
		if (pluginConf.enabled === false) continue;

		// Auto-install from source if needed
		if (pluginConf.source) {
			const installDir = join(gitagentDir, "plugins");
			try {
				await installPlugin(pluginConf.source, installDir, pluginConf.version);
			} catch (err: any) {
				console.warn(`Plugin "${pluginName}": install failed: ${err.message}`);
				continue;
			}
		}

		// Discover plugin directory
		const pluginDir = await discoverPluginDirs(pluginName, agentDir, gitagentDir);
		if (!pluginDir) {
			console.warn(`Plugin "${pluginName}": not found in any plugin directory`);
			continue;
		}

		// Load plugin
		const plugin = await loadPlugin(pluginDir, pluginConf);
		if (!plugin) continue;

		// Check for tool name collisions (declarative + programmatic)
		const allPluginToolNames = [
			...plugin.tools.map((t) => t.name),
			...plugin.programmaticTools.map((t) => t.name),
		];
		const collisions = allPluginToolNames.filter((name) => toolNames.has(name));
		if (collisions.length > 0) {
			console.error(`Plugin "${pluginName}": tool name collision(s): ${collisions.join(", ")}. Skipping plugin.`);
			continue;
		}
		for (const name of allPluginToolNames) {
			toolNames.add(name);
		}

		loaded.push(plugin);
	}

	return loaded;
}

// ── Hook merging ───────────────────────────────────────────────────────

export function mergeHooksConfigs(
	base: HooksConfig | null,
	plugins: LoadedPlugin[],
): HooksConfig | null {
	const merged: HooksConfig = {
		hooks: {
			on_session_start: [...(base?.hooks.on_session_start || [])],
			pre_tool_use: [...(base?.hooks.pre_tool_use || [])],
			post_response: [...(base?.hooks.post_response || [])],
			on_error: [...(base?.hooks.on_error || [])],
		},
	};

	for (const plugin of plugins) {
		if (!plugin.hooks) continue;
		for (const event of ["on_session_start", "pre_tool_use", "post_response", "on_error"] as const) {
			const pluginHooks = plugin.hooks.hooks[event];
			if (pluginHooks) {
				merged.hooks[event] = [...(merged.hooks[event] || []), ...pluginHooks];
			}
		}
	}

	const hasAny = Object.values(merged.hooks).some((arr) => arr && arr.length > 0);
	return hasAny ? merged : null;
}

// ── List all discoverable plugins (for CLI) ────────────────────────────

export interface DiscoveredPlugin {
	name: string;
	version: string;
	description: string;
	scope: "local" | "global" | "installed";
	directory: string;
}

export async function listAllPlugins(
	agentDir: string,
	gitagentDir: string,
): Promise<DiscoveredPlugin[]> {
	const plugins: DiscoveredPlugin[] = [];

	const scopes: Array<{ dir: string; scope: "local" | "global" | "installed" }> = [
		{ dir: join(agentDir, "plugins"), scope: "local" },
		{ dir: join(homedir(), ".gitclaw", "plugins"), scope: "global" },
		{ dir: join(gitagentDir, "plugins"), scope: "installed" },
	];

	for (const { dir, scope } of scopes) {
		if (!(await dirExists(dir))) continue;

		const entries = await readdir(dir, { withFileTypes: true });
		for (const entry of entries) {
			if (!entry.isDirectory()) continue;

			const pluginDir = join(dir, entry.name);
			const manifestPath = join(pluginDir, "plugin.yaml");
			try {
				const raw = await readFile(manifestPath, "utf-8");
				const manifest = yaml.load(raw) as any;
				if (manifest?.id && manifest?.version && manifest?.description) {
					plugins.push({
						name: manifest.id,
						version: manifest.version,
						description: manifest.description,
						scope,
						directory: pluginDir,
					});
				}
			} catch {
				// Skip invalid plugins
			}
		}
	}

	return plugins;
}
