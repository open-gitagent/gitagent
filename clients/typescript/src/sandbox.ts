/**
 * 1.x's remote sandbox: the agent's commands and file edits run in a VM
 * (E2B, through the optional `gitmachine` package) holding a clone of the
 * repository, while the agent itself runs here. API keys stay on this
 * machine. The tools below take the place of the built-in ones in a query
 * with the `sandbox` option, under 1.x's names: cli, read, write, edit and
 * memory.
 */

import { execSync } from "node:child_process";
import type { GCToolDefinition, JsonObject } from "./index.js";

export interface SandboxOptions {
  provider?: "e2b";
  template?: string;
  /** VM lifetime in seconds, as gitmachine takes it. */
  timeout?: number;
  /** The repository to clone (default: the folder's `origin`). */
  repository?: string;
  /** Token for cloning and pushing (default: GITHUB_TOKEN or GIT_TOKEN). */
  token?: string;
  /** A session branch from an earlier run to continue. */
  session?: string;
  /** Default true: commit the work when the sandbox stops. */
  autoCommit?: boolean;
  envs?: Record<string, string>;
  /** Called with a line of progress: starting, ready, stopping. */
  onStatus?: (message: string) => void;
}

/** 1.x's name for the same settings. */
export type SandboxConfig = SandboxOptions;

/** A started or starting sandbox: gitmachine's GitMachine and Machine. */
export interface SandboxContext {
  /** gitmachine's GitMachine: run(), commit(), start(), stop(). */
  gitMachine: any;
  /** gitmachine's Machine: readFile(), writeFile(). */
  machine: any;
  /** The repository's root inside the VM, e.g. /home/user/app. */
  repoPath: string;
}

const MAX_OUTPUT = 100_000;
const MAX_LINES = 2000;
const DEFAULT_TIMEOUT = 120;
const DEFAULT_MEMORY_PATH = "memory/MEMORY.md";

function detectRepoUrl(dir: string): string | null {
  try {
    return execSync("git remote get-url origin", { cwd: dir, stdio: "pipe" }).toString().trim();
  } catch {
    return null;
  }
}

/** Prepares a sandbox (not started yet) with gitmachine. */
export async function createSandboxContext(config: SandboxOptions, dir: string): Promise<SandboxContext> {
  // gitmachine is an optional peer dependency; the name is a variable so
  // the build does not need it installed.
  const name = "gitmachine";
  let gitmachine: any;
  try {
    gitmachine = await import(name);
  } catch {
    throw new Error("Sandbox mode needs the 'gitmachine' package. Install it with: npm install -g gitmachine");
  }
  const token = config.token || process.env.GITHUB_TOKEN || process.env.GIT_TOKEN;
  const repository = config.repository || detectRepoUrl(dir);
  if (!repository) {
    throw new Error(
      "Sandbox mode needs a repository: pass --sandbox-repo (or `repository`), or run in a folder whose git remote `origin` is set.",
    );
  }
  const gitMachine = new gitmachine.GitMachine({
    provider: config.provider ?? "e2b",
    template: config.template,
    timeout: config.timeout,
    repository,
    token,
    session: config.session,
    autoCommit: config.autoCommit ?? true,
    envs: config.envs,
  });
  const repoName = repository.split("/").pop()?.replace(/\.git$/, "") || "repo";
  return { gitMachine, machine: gitMachine.machine, repoPath: `/home/user/${repoName}` };
}

function inRepo(path: string, repoRoot: string): string {
  if (path === "~" || path.startsWith("~/")) path = "/home/user" + path.slice(1);
  if (path.startsWith("/")) return path;
  return repoRoot.endsWith("/") ? repoRoot + path : `${repoRoot}/${path}`;
}

function quote(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

function failed(text: string): JsonObject {
  return { content: text, isError: true };
}

function str(args: JsonObject, key: string): string | undefined {
  const v = args[key];
  return typeof v === "string" ? v : undefined;
}

function num(args: JsonObject, key: string): number | undefined {
  const v = args[key];
  return typeof v === "number" ? v : undefined;
}

/** The sandbox's tools; each waits for `sandbox` before it runs. */
export function sandboxTools(sandbox: Promise<SandboxContext>): GCToolDefinition[] {
  const run = async (command: string, timeout?: number): Promise<{ output: string; exitCode: number }> => {
    const ctx = await sandbox;
    let output = "";
    const result = await ctx.gitMachine.run(command, {
      cwd: ctx.repoPath,
      timeout: timeout ?? DEFAULT_TIMEOUT,
      onStdout: (data: string) => (output += data),
      onStderr: (data: string) => (output += data),
    });
    return { output, exitCode: result?.exitCode ?? 0 };
  };

  const cli: GCToolDefinition = {
    name: "cli",
    description:
      "Execute a shell command in the sandbox VM. Returns stdout and stderr combined. Output is truncated if it exceeds ~100KB. Default timeout is 120 seconds.",
    inputSchema: {
      properties: {
        command: { type: "string", description: "Shell command to execute" },
        timeout: { type: "number", description: "Timeout in seconds (default: 120)" },
      },
      required: ["command"],
    },
    handler: async (args, signal) => {
      if (signal.aborted) return failed("Operation aborted");
      const command = str(args, "command");
      if (!command) return failed("command is required");
      const { output, exitCode } = await run(command, num(args, "timeout"));
      let text =
        output.length > MAX_OUTPUT
          ? `[output truncated, showing last ~100KB]\n${output.slice(-MAX_OUTPUT)}`
          : output || "(no output)";
      if (exitCode !== 0) {
        text += `\n\nExit code: ${exitCode}`;
        return failed(text);
      }
      return text;
    },
  };

  const read: GCToolDefinition = {
    name: "read",
    description: "Read a file in the sandbox VM. Long files are shown in pages; use offset and limit for the rest.",
    inputSchema: {
      properties: {
        path: { type: "string", description: "Path to the file to read (relative or absolute)" },
        offset: { type: "number", description: "Line number to start from (1-indexed)" },
        limit: { type: "number", description: "Maximum number of lines to read" },
      },
      required: ["path"],
    },
    handler: async (args) => {
      const path = str(args, "path");
      if (!path) return failed("path is required");
      const ctx = await sandbox;
      let text: string;
      try {
        text = await ctx.machine.readFile(inRepo(path, ctx.repoPath));
      } catch (e) {
        return failed(`Cannot read ${path}: ${(e as Error).message}`);
      }
      const lines = text.split("\n");
      const offset = num(args, "offset");
      const start = offset ? Math.max(0, offset - 1) : 0;
      if (start >= lines.length) return failed(`Offset ${offset} is beyond end of file (${lines.length} lines)`);
      const end = Math.min(start + (num(args, "limit") ?? MAX_LINES), lines.length);
      let shown = lines.slice(start, end).join("\n");
      let cut = false;
      if (Buffer.byteLength(shown, "utf-8") > MAX_OUTPUT) {
        shown = shown.slice(0, MAX_OUTPUT);
        cut = true;
      }
      if (end < lines.length || cut) {
        shown += `\n\n[Showing lines ${start + 1}-${end} of ${lines.length}. Use offset=${end + 1} to continue.]`;
      }
      return shown;
    },
  };

  const write: GCToolDefinition = {
    name: "write",
    description: "Write a file in the sandbox VM, creating parent directories as needed.",
    inputSchema: {
      properties: {
        path: { type: "string", description: "Path to the file to write (relative or absolute)" },
        content: { type: "string", description: "Content to write to the file" },
        createDirs: { type: "boolean", description: "Create parent directories if needed (default: true)" },
      },
      required: ["path", "content"],
    },
    handler: async (args) => {
      const path = str(args, "path");
      const content = str(args, "content");
      if (!path || content === undefined) return failed("path and content are required");
      const ctx = await sandbox;
      const target = inRepo(path, ctx.repoPath);
      const dir = target.substring(0, target.lastIndexOf("/"));
      if (args.createDirs !== false && dir) await run(`mkdir -p ${quote(dir)}`);
      await ctx.machine.writeFile(target, content);
      return `Wrote ${Buffer.byteLength(content, "utf-8")} bytes to ${path}`;
    },
  };

  const edit: GCToolDefinition = {
    name: "edit",
    description:
      "Replace text in a file in the sandbox VM. old_string must match exactly once unless replace_all is true; with regex, old_string is a JavaScript regular expression.",
    inputSchema: {
      properties: {
        path: { type: "string", description: "Path to the file to edit (relative or absolute)" },
        old_string: { type: "string", description: "Exact text to find and replace. Must match uniquely unless replace_all is true." },
        new_string: { type: "string", description: "Replacement text" },
        replace_all: { type: "boolean", description: "Replace every occurrence (default: false)" },
        regex: { type: "boolean", description: "Treat old_string as a JavaScript regular expression (default: false). When true, new_string may reference groups like $1." },
        flags: { type: "string", description: "Regex flags (e.g. 'i', 'm', 's'). Only used when regex=true. 'g' is added automatically when replace_all is true." },
      },
      required: ["path", "old_string", "new_string"],
    },
    handler: async (args) => {
      const path = str(args, "path");
      const oldString = str(args, "old_string");
      const newString = str(args, "new_string");
      if (!path || oldString === undefined || newString === undefined) {
        return failed("path, old_string and new_string are required");
      }
      if (oldString === newString) return failed("old_string and new_string are identical — nothing to change");
      const replaceAll = args.replace_all === true;
      const ctx = await sandbox;
      const target = inRepo(path, ctx.repoPath);
      let original: string;
      try {
        original = await ctx.machine.readFile(target);
      } catch (e) {
        return failed(`Cannot read ${path}: ${(e as Error).message}`);
      }
      let updated: string;
      let count: number;
      if (args.regex === true) {
        let flags = str(args, "flags") ?? "";
        if (replaceAll && !flags.includes("g")) flags += "g";
        let rx: RegExp;
        try {
          rx = new RegExp(oldString, flags);
        } catch (e) {
          return failed(`Invalid regex: ${(e as Error).message}`);
        }
        count = original.match(new RegExp(oldString, flags.includes("g") ? flags : flags + "g"))?.length ?? 0;
        if (count === 0) return failed(`Regex pattern not found in ${path}`);
        if (!replaceAll && count > 1) {
          return failed(`Regex matched ${count} times in ${path}. Make the pattern more specific or set replace_all=true.`);
        }
        updated = original.replace(rx, newString);
      } else {
        if (!oldString) return failed("old_string cannot be empty");
        count = original.split(oldString).length - 1;
        if (count === 0) return failed(`old_string not found in ${path}`);
        if (!replaceAll && count > 1) {
          return failed(
            `old_string matches ${count} times in ${path}. Provide more surrounding context to make it unique, or set replace_all=true.`,
          );
        }
        updated = replaceAll ? original.split(oldString).join(newString) : original.replace(oldString, () => newString);
      }
      if (updated === original) return failed("No changes applied — replacement produced identical content");
      await ctx.machine.writeFile(target, updated);
      const applied = replaceAll ? count : 1;
      return `Edited ${path} — ${applied} replacement${applied === 1 ? "" : "s"} applied`;
    },
  };

  const memory: GCToolDefinition = {
    name: "memory",
    description:
      "Git-backed memory in the sandbox VM. Use 'load' to read current memory, 'save' to update memory and commit to git. Each save creates a git commit.",
    inputSchema: {
      properties: {
        action: { type: "string", enum: ["load", "save"], description: "Whether to load or save memory" },
        content: { type: "string", description: "Memory content to save (required for save)" },
        message: { type: "string", description: "Commit message describing why this memory changed (required for save)" },
      },
      required: ["action"],
    },
    handler: async (args) => {
      const ctx = await sandbox;
      const layer = await workingLayer(ctx);
      const file = inRepo(layer.path, ctx.repoPath);
      if (args.action === "load") {
        try {
          const text = String(await ctx.machine.readFile(file)).trim();
          return !text || text === "# Memory" ? "No memories yet." : text;
        } catch {
          return "No memories yet.";
        }
      }
      if (args.action !== "save") return failed("action must be load or save");
      const content = str(args, "content");
      if (!content) return failed("content is required for save action");
      const message = str(args, "message") || "Update memory";
      let kept = content;
      if (layer.maxLines) kept = await archiveOverflow(ctx, content, layer.maxLines, run);
      const dir = file.substring(0, file.lastIndexOf("/"));
      if (dir) await run(`mkdir -p ${quote(dir)}`);
      await ctx.machine.writeFile(file, kept);
      try {
        await ctx.gitMachine.commit(message);
      } catch (e) {
        return `Memory saved to ${layer.path} but git commit failed: ${(e as Error).message || "unknown error"}. The file was still written.`;
      }
      return `Memory saved and committed: "${message}"`;
    },
  };

  return [cli, read, write, edit, memory];
}

/** The working layer of `memory/memory.yaml` in the VM: its path and line limit. */
async function workingLayer(ctx: SandboxContext): Promise<{ path: string; maxLines?: number }> {
  let raw: string;
  try {
    raw = String(await ctx.machine.readFile(inRepo("memory/memory.yaml", ctx.repoPath)));
  } catch {
    return { path: DEFAULT_MEMORY_PATH };
  }
  // The layers are a YAML list of flat mappings; read name, path and max_lines of each.
  const layers = raw
    .split(/^\s*-\s+/m)
    .slice(1)
    .map((item) => {
      const field = (key: string) => item.match(new RegExp(`^\\s*${key}:\\s*["']?([^"'\\n#]+?)["']?\\s*(#.*)?$`, "m"))?.[1];
      const maxLines = Number(field("max_lines"));
      return { name: field("name"), path: field("path"), maxLines: Number.isFinite(maxLines) && maxLines > 0 ? maxLines : undefined };
    })
    .filter((l) => l.path);
  const working = layers.find((l) => l.name === "working") ?? layers[0];
  return working?.path ? { path: working.path, maxLines: working.maxLines } : { path: DEFAULT_MEMORY_PATH };
}

/** Moves the lines over `maxLines` to this month's archive file and gives the rest. */
async function archiveOverflow(
  ctx: SandboxContext,
  content: string,
  maxLines: number,
  run: (command: string) => Promise<unknown>,
): Promise<string> {
  const lines = content.split("\n");
  if (lines.length <= maxLines) return content;
  const overflow = lines.slice(0, lines.length - maxLines).join("\n");
  const kept = lines.slice(lines.length - maxLines).join("\n");
  const now = new Date();
  const archive = inRepo(
    `memory/archive/${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}.md`,
    ctx.repoPath,
  );
  await run(`mkdir -p ${quote(archive.substring(0, archive.lastIndexOf("/")))}`);
  let existing = "";
  try {
    existing = String(await ctx.machine.readFile(archive));
  } catch {
    // A new archive file.
  }
  await ctx.machine.writeFile(archive, `${existing}\n---\n_Archived: ${now.toISOString()}_\n\n${overflow}\n`);
  return kept;
}
