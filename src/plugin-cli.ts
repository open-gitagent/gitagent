import { readFile, writeFile, mkdir, rm, cp, stat } from "fs/promises";
import { join, resolve } from "path";
import { execSync } from "child_process";
import yaml from "js-yaml";
// "yaml" (v2) is used here instead of js-yaml because parseDocument()
// preserves comments and formatting when editing agent.yaml.
import { parseDocument } from "yaml";
import { installPlugin, listAllPlugins } from "./plugins.js";

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string) => `\x1b[1m${s}\x1b[0m`;
const red = (s: string) => `\x1b[31m${s}\x1b[0m`;
const green = (s: string) => `\x1b[32m${s}\x1b[0m`;

async function fileExists(path: string): Promise<boolean> {
	try {
		await stat(path);
		return true;
	} catch {
		return false;
	}
}

async function dirExists(path: string): Promise<boolean> {
	try {
		const s = await stat(path);
		return s.isDirectory();
	} catch {
		return false;
	}
}

async function ensureGitagentDir(agentDir: string): Promise<string> {
	const gitagentDir = join(agentDir, ".gitagent");
	await mkdir(gitagentDir, { recursive: true });
	return gitagentDir;
}

// ── Install command ────────────────────────────────────────────────────

async function handleInstall(agentDir: string, args: string[]): Promise<void> {
	const source = args[0];
	if (!source || source.startsWith("--")) {
		console.error(red("Usage: gitclaw plugin install <source> [--name <name>] [--force] [--no-enable]"));
		console.error(dim("  source: git URL or local path"));
		process.exit(1);
	}

	let name: string | undefined;
	const nameIdx = args.indexOf("--name");
	if (nameIdx !== -1 && args[nameIdx + 1]) {
		name = args[nameIdx + 1];
	}
	const force = args.includes("--force");
	const noEnable = args.includes("--no-enable");

	// Determine if source is a git URL or local path
	const isGitUrl = source.startsWith("http://") || source.startsWith("https://") || source.startsWith("git@") || source.endsWith(".git");

	if (isGitUrl) {
		// Install to .gitagent/plugins/
		const gitagentDir = await ensureGitagentDir(agentDir);
		const installDir = join(gitagentDir, "plugins");
		try {
			const pluginDir = await installPlugin(source, installDir, undefined, force);
			const pluginName = name || pluginDir.split("/").pop()!;
			console.log(green(`Installed plugin "${pluginName}" from ${source}`));
			console.log(dim(`Location: ${pluginDir}`));

			// Add to agent.yaml
			await addPluginToManifest(agentDir, pluginName, { source, enabled: !noEnable });
			console.log(dim(`Added to agent.yaml`));
		} catch (err: any) {
			console.error(red(`Install failed: ${err.message}`));
			process.exit(1);
		}
	} else {
		// Local path: copy to plugins/
		const sourcePath = resolve(source);
		if (!(await dirExists(sourcePath))) {
			console.error(red(`Source path does not exist: ${sourcePath}`));
			process.exit(1);
		}

		const pluginName = name || sourcePath.split("/").pop()!;
		const targetDir = join(agentDir, "plugins", pluginName);

		if (force && await dirExists(targetDir)) {
			await rm(targetDir, { recursive: true, force: true });
		}

		await mkdir(join(agentDir, "plugins"), { recursive: true });
		await cp(sourcePath, targetDir, { recursive: true });

		console.log(green(`Installed plugin "${pluginName}" from ${sourcePath}`));
		console.log(dim(`Location: ${targetDir}`));

		await addPluginToManifest(agentDir, pluginName, { enabled: !noEnable });
		console.log(dim(`Added to agent.yaml`));
	}
}

// ── List command ───────────────────────────────────────────────────────

async function handleList(agentDir: string): Promise<void> {
	const gitagentDir = join(agentDir, ".gitagent");
	const plugins = await listAllPlugins(agentDir, gitagentDir);

	if (plugins.length === 0) {
		console.log(dim("No plugins found."));
		return;
	}

	// Read agent.yaml to check enabled status
	let manifest: any = {};
	try {
		const raw = await readFile(join(agentDir, "agent.yaml"), "utf-8");
		manifest = yaml.load(raw) as any;
	} catch { /* no manifest */ }

	console.log(bold("Plugins:"));
	for (const p of plugins) {
		const enabled = manifest.plugins?.[p.name]?.enabled !== false;
		const status = enabled ? green("enabled") : dim("disabled");
		const scope = dim(`(${p.scope})`);
		console.log(`  ${bold(p.name)} v${p.version} ${scope} ${status} — ${dim(p.description)}`);
	}
}

// ── Remove command ─────────────────────────────────────────────────────

async function handleRemove(agentDir: string, args: string[]): Promise<void> {
	const name = args[0];
	if (!name) {
		console.error(red("Usage: gitclaw plugin remove <name>"));
		process.exit(1);
	}

	// Try local first, then installed
	const localDir = join(agentDir, "plugins", name);
	const installedDir = join(agentDir, ".gitagent", "plugins", name);

	let removed = false;
	if (await dirExists(localDir)) {
		await rm(localDir, { recursive: true, force: true });
		console.log(green(`Removed local plugin "${name}"`));
		removed = true;
	} else if (await dirExists(installedDir)) {
		await rm(installedDir, { recursive: true, force: true });
		console.log(green(`Removed installed plugin "${name}"`));
		removed = true;
	}

	if (!removed) {
		console.error(red(`Plugin "${name}" not found`));
		process.exit(1);
	}

	await removePluginFromManifest(agentDir, name);
	console.log(dim("Removed from agent.yaml"));
}

// ── Enable/disable commands ────────────────────────────────────────────

async function handleToggle(agentDir: string, args: string[], enabled: boolean): Promise<void> {
	const name = args[0];
	if (!name) {
		console.error(red(`Usage: gitclaw plugin ${enabled ? "enable" : "disable"} <name>`));
		process.exit(1);
	}

	await addPluginToManifest(agentDir, name, { enabled });
	const action = enabled ? "Enabled" : "Disabled";
	console.log(green(`${action} plugin "${name}"`));
}

// ── Init command ───────────────────────────────────────────────────────

async function handleInit(agentDir: string, args: string[]): Promise<void> {
	const name = args[0];
	if (!name) {
		console.error(red("Usage: gitclaw plugin init <name>"));
		process.exit(1);
	}

	if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(name)) {
		console.error(red("Plugin name must be kebab-case (e.g., my-plugin)"));
		process.exit(1);
	}

	const pluginDir = join(agentDir, "plugins", name);
	if (await dirExists(pluginDir)) {
		console.error(red(`Plugin "${name}" already exists at ${pluginDir}`));
		process.exit(1);
	}

	await mkdir(pluginDir, { recursive: true });
	await mkdir(join(pluginDir, "tools"), { recursive: true });
	await mkdir(join(pluginDir, "hooks"), { recursive: true });
	await mkdir(join(pluginDir, "skills"), { recursive: true });

	const manifest = [
		`id: ${name}`,
		`name: ${name}`,
		"version: 0.1.0",
		`description: A gitclaw plugin`,
		"",
		"provides:",
		"  tools: true",
		"  skills: true",
		"  # hooks:",
		"  #   pre_tool_use:",
		"  #     - script: hooks/check.sh",
		"  #       description: Check tool input",
		"  # prompt: prompt.md",
		"",
		"# config:",
		"#   properties:",
		"#     api_key:",
		"#       type: string",
		"#       description: API key",
		"#       env: MY_API_KEY",
		"#   required: [api_key]",
		"",
	].join("\n");

	await writeFile(join(pluginDir, "plugin.yaml"), manifest, "utf-8");
	await writeFile(join(pluginDir, "README.md"), `# ${name}\n\nA gitclaw plugin.\n`, "utf-8");

	console.log(green(`Created plugin "${name}" at ${pluginDir}`));
	console.log(dim("Files:"));
	console.log(dim("  plugin.yaml    — Plugin manifest"));
	console.log(dim("  tools/         — Declarative tool definitions"));
	console.log(dim("  hooks/         — Hook scripts"));
	console.log(dim("  skills/        — Skill modules"));

	await addPluginToManifest(agentDir, name, { enabled: true });
	console.log(dim("Added to agent.yaml"));
}

// ── agent.yaml helpers ─────────────────────────────────────────────────

async function addPluginToManifest(
	agentDir: string,
	name: string,
	pluginConf: Record<string, any>,
): Promise<void> {
	const manifestPath = join(agentDir, "agent.yaml");
	try {
		const raw = await readFile(manifestPath, "utf-8");
		const doc = parseDocument(raw);

		if (!doc.has("plugins")) {
			doc.set("plugins", {});
		}
		const pluginsNode = doc.get("plugins", true) as any;

		const existing = pluginsNode?.get?.(name, true);
		if (existing && typeof existing.toJSON === "function") {
			const merged = { ...existing.toJSON(), ...pluginConf };
			pluginsNode.set(name, merged);
		} else {
			pluginsNode.set(name, pluginConf);
		}

		await writeFile(manifestPath, doc.toString(), "utf-8");
	} catch (err: any) {
		console.error(`Failed to update agent.yaml: ${err.message}`);
	}
}

async function removePluginFromManifest(agentDir: string, name: string): Promise<void> {
	const manifestPath = join(agentDir, "agent.yaml");
	try {
		const raw = await readFile(manifestPath, "utf-8");
		const doc = parseDocument(raw);

		const pluginsNode = doc.get("plugins", true) as any;
		if (pluginsNode?.has?.(name)) {
			pluginsNode.delete(name);
			// Remove empty plugins key
			if (pluginsNode.items?.length === 0) {
				doc.delete("plugins");
			}
			await writeFile(manifestPath, doc.toString(), "utf-8");
		}
	} catch (err: any) {
		console.error(`Failed to update agent.yaml: ${err.message}`);
	}
}

// ── Main CLI handler ───────────────────────────────────────────────────

export async function handlePluginCommand(agentDir: string, args: string[]): Promise<void> {
	const subcommand = args[0];
	const subArgs = args.slice(1);

	switch (subcommand) {
		case "install":
			await handleInstall(agentDir, subArgs);
			break;
		case "list":
		case "ls":
			await handleList(agentDir);
			break;
		case "remove":
		case "rm":
			await handleRemove(agentDir, subArgs);
			break;
		case "enable":
			await handleToggle(agentDir, subArgs, true);
			break;
		case "disable":
			await handleToggle(agentDir, subArgs, false);
			break;
		case "init":
		case "create":
			await handleInit(agentDir, subArgs);
			break;
		default:
			console.log(bold("gitclaw plugin") + " — Plugin management\n");
			console.log("Commands:");
			console.log(`  ${bold("install")} <source>    Install a plugin (git URL or local path)`);
			console.log(`  ${bold("list")}               List all discovered plugins`);
			console.log(`  ${bold("remove")} <name>      Remove a plugin`);
			console.log(`  ${bold("enable")} <name>      Enable a plugin`);
			console.log(`  ${bold("disable")} <name>     Disable a plugin`);
			console.log(`  ${bold("init")} <name>        Scaffold a new plugin`);
			break;
	}
}
