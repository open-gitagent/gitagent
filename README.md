<p align="center">
  <img src="./gitclaw-logo.png" alt="GitClaw Logo" width="200" />
</p>

<p align="center">
  <img src="https://img.shields.io/npm/v/gitclaw?style=flat-square&color=blue" alt="npm version" />
  <img src="https://img.shields.io/badge/node-%3E%3D20-brightgreen?style=flat-square" alt="node version" />
  <img src="https://img.shields.io/github/license/open-gitagent/gitclaw?style=flat-square" alt="license" />
  <img src="https://img.shields.io/badge/TypeScript-5.7-blue?style=flat-square&logo=typescript&logoColor=white" alt="typescript" />
</p>

<h1 align="center">Gitclaw</h1>

<p align="center">
  <strong>A universal git-native multimodal always learning AI Agent (TinyHuman)</strong><br/>
  Your agent lives inside a git repo — identity, rules, memory, tools, and skills are all version-controlled files.
</p>

<p align="center">
  <a href="#one-command-install">Install</a> &bull;
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#sdk">SDK</a> &bull;
  <a href="#architecture">Architecture</a> &bull;
  <a href="#tools">Tools</a> &bull;
  <a href="#hooks">Hooks</a> &bull;
  <a href="#skills">Skills</a> &bull;
  <a href="#plugins">Plugins</a>
</p>

---

## Why Gitclaw?

Most agent frameworks treat configuration as code scattered across your application. Gitclaw flips this — **your agent IS a git repository**:

- **`agent.yaml`** — model, tools, runtime config
- **`SOUL.md`** — personality and identity
- **`RULES.md`** — behavioral constraints
- **`memory/`** — git-committed memory with full history
- **`tools/`** — declarative YAML tool definitions
- **`skills/`** — composable skill modules
- **`hooks/`** — lifecycle hooks (script or programmatic)

Fork an agent. Branch a personality. `git log` your agent's memory. Diff its rules. This is **agents as repos**.

## One-Command Install

Copy, paste, run. That's it — no cloning, no manual setup. The installer handles everything:

```bash
bash <(curl -fsSL "https://raw.githubusercontent.com/open-gitagent/gitclaw/main/install.sh?$(date +%s)")
```

This will:
- Install gitclaw globally via npm
- Walk you through API key setup (Quick or Advanced mode)
- Launch the voice UI in your browser at `http://localhost:3333`

> **Requirements:** Node.js 18+, npm, git

### Or install manually:

```bash
npm install -g gitclaw
```

## Quick Start

**Run your first agent in one line:**

```bash
export OPENAI_API_KEY="sk-..."
gitclaw --dir ~/my-project "Explain this project and suggest improvements"
```

That's it. Gitclaw auto-scaffolds everything on first run — `agent.yaml`, `SOUL.md`, `memory/` — and drops you into the agent.

### Local Repo Mode

Clone a GitHub repo, run an agent on it, auto-commit and push to a session branch:

```bash
gitclaw --repo https://github.com/org/repo --pat ghp_xxx "Fix the login bug"
```

Resume an existing session:

```bash
gitclaw --repo https://github.com/org/repo --pat ghp_xxx --session gitclaw/session-a1b2c3d4 "Continue"
```

Token can come from env instead of `--pat`:

```bash
export GITHUB_TOKEN=ghp_xxx
gitclaw --repo https://github.com/org/repo "Add unit tests"
```

### CLI Options

| Flag | Short | Description |
|---|---|---|
| `--dir <path>` | `-d` | Agent directory (default: cwd) |
| `--repo <url>` | `-r` | GitHub repo URL to clone and work on |
| `--pat <token>` | | GitHub PAT (or set `GITHUB_TOKEN` / `GIT_TOKEN`) |
| `--session <branch>` | | Resume an existing session branch |
| `--model <provider:model>` | `-m` | Override model (e.g. `anthropic:claude-sonnet-4-5-20250929`) |
| `--sandbox` | `-s` | Run in sandbox VM |
| `--prompt <text>` | `-p` | Single-shot prompt (skip REPL) |
| `--env <name>` | `-e` | Environment config |

### SDK

```bash
npm install gitclaw
```

```typescript
import { query } from "gitclaw";

// Simple query
for await (const msg of query({
  prompt: "List all TypeScript files and summarize them",
  dir: "./my-agent",
  model: "openai:gpt-4o-mini",
})) {
  if (msg.type === "delta") process.stdout.write(msg.content);
  if (msg.type === "assistant") console.log("\n\nDone.");
}

// Local repo mode via SDK
for await (const msg of query({
  prompt: "Fix the login bug",
  model: "openai:gpt-4o-mini",
  repo: {
    url: "https://github.com/org/repo",
    token: process.env.GITHUB_TOKEN!,
  },
})) {
  if (msg.type === "delta") process.stdout.write(msg.content);
}
```

## SDK

The SDK provides a programmatic interface to Gitclaw agents. It mirrors the [Claude Agent SDK](https://github.com/anthropics/claude-code-sdk) pattern but runs **in-process** — no subprocesses, no IPC.

### `query(options): Query`

Returns an `AsyncGenerator<GCMessage>` that streams agent events.

```typescript
import { query } from "gitclaw";

for await (const msg of query({
  prompt: "Refactor the auth module",
  dir: "/path/to/agent",
  model: "anthropic:claude-sonnet-4-5-20250929",
})) {
  switch (msg.type) {
    case "delta":       // streaming text chunk
      process.stdout.write(msg.content);
      break;
    case "assistant":   // complete response
      console.log(`\nTokens: ${msg.usage?.totalTokens}`);
      break;
    case "tool_use":    // tool invocation
      console.log(`Tool: ${msg.toolName}(${JSON.stringify(msg.args)})`);
      break;
    case "tool_result": // tool output
      console.log(`Result: ${msg.content}`);
      break;
    case "system":      // lifecycle events & errors
      console.log(`[${msg.subtype}] ${msg.content}`);
      break;
  }
}
```

### `tool(name, description, schema, handler): GCToolDefinition`

Define custom tools the agent can call:

```typescript
import { query, tool } from "gitclaw";

const search = tool(
  "search_docs",
  "Search the documentation",
  {
    properties: {
      query: { type: "string", description: "Search query" },
      limit: { type: "number", description: "Max results" },
    },
    required: ["query"],
  },
  async (args) => {
    const results = await mySearchEngine(args.query, args.limit ?? 10);
    return { text: JSON.stringify(results), details: { count: results.length } };
  },
);

for await (const msg of query({
  prompt: "Find docs about authentication",
  tools: [search],
})) {
  // agent can now call search_docs
}
```

### Hooks

Programmatic lifecycle hooks for gating, logging, and control:

```typescript
for await (const msg of query({
  prompt: "Deploy the service",
  hooks: {
    preToolUse: async (ctx) => {
      // Block dangerous operations
      if (ctx.toolName === "cli" && ctx.args.command?.includes("rm -rf"))
        return { action: "block", reason: "Destructive command blocked" };

      // Modify arguments
      if (ctx.toolName === "write" && !ctx.args.path.startsWith("/safe/"))
        return { action: "modify", args: { ...ctx.args, path: `/safe/${ctx.args.path}` } };

      return { action: "allow" };
    },
    onError: async (ctx) => {
      console.error(`Agent error: ${ctx.error}`);
    },
  },
})) {
  // ...
}
```

### QueryOptions Reference

| Option | Type | Description |
|---|---|---|
| `prompt` | `string \| AsyncIterable` | User prompt or multi-turn stream |
| `dir` | `string` | Agent directory (default: `cwd`) |
| `model` | `string` | `"provider:model-id"` |
| `env` | `string` | Environment config (`config/<env>.yaml`) |
| `systemPrompt` | `string` | Override discovered system prompt |
| `systemPromptSuffix` | `string` | Append to discovered system prompt |
| `tools` | `GCToolDefinition[]` | Additional tools |
| `replaceBuiltinTools` | `boolean` | Skip cli/read/write/memory |
| `allowedTools` | `string[]` | Tool name allowlist |
| `disallowedTools` | `string[]` | Tool name denylist |
| `repo` | `LocalRepoOptions` | Clone a GitHub repo and work on a session branch |
| `sandbox` | `SandboxOptions \| boolean` | Run in sandbox VM (mutually exclusive with `repo`) |
| `hooks` | `GCHooks` | Programmatic lifecycle hooks |
| `maxTurns` | `number` | Max agent turns |
| `abortController` | `AbortController` | Cancellation signal |
| `constraints` | `object` | `temperature`, `maxTokens`, `topP`, `topK` |

### Message Types

| Type | Description | Key Fields |
|---|---|---|
| `delta` | Streaming text/thinking chunk | `deltaType`, `content` |
| `assistant` | Complete LLM response | `content`, `model`, `usage`, `stopReason` |
| `tool_use` | Tool invocation | `toolName`, `args`, `toolCallId` |
| `tool_result` | Tool output | `content`, `isError`, `toolCallId` |
| `system` | Lifecycle events | `subtype`, `content`, `metadata` |
| `user` | User message (multi-turn) | `content` |

## Architecture

```
my-agent/
├── agent.yaml          # Model, tools, runtime config
├── SOUL.md             # Agent identity & personality
├── RULES.md            # Behavioral rules & constraints
├── DUTIES.md           # Role-specific responsibilities
├── memory/
│   └── MEMORY.md       # Git-committed agent memory
├── tools/
│   └── *.yaml          # Declarative tool definitions
├── skills/
│   └── <name>/
│       ├── SKILL.md    # Skill instructions (YAML frontmatter)
│       └── scripts/    # Skill scripts
├── workflows/
│   └── *.yaml|*.md     # Multi-step workflow definitions
├── agents/
│   └── <name>/         # Sub-agent definitions
├── plugins/
│   └── <name>/         # Local plugins (plugin.yaml + tools/hooks/skills)
├── hooks/
│   └── hooks.yaml      # Lifecycle hook scripts
├── knowledge/
│   └── index.yaml      # Knowledge base entries
├── config/
│   ├── default.yaml    # Default environment config
│   └── <env>.yaml      # Environment overrides
├── examples/
│   └── *.md            # Few-shot examples
└── compliance/
    └── *.yaml          # Compliance & audit config
```

### Agent Manifest (`agent.yaml`)

```yaml
spec_version: "0.1.0"
name: my-agent
version: 1.0.0
description: An agent that does things

model:
  preferred: "anthropic:claude-sonnet-4-5-20250929"
  fallback: ["openai:gpt-4o"]
  constraints:
    temperature: 0.7
    max_tokens: 4096

tools: [cli, read, write, memory]

runtime:
  max_turns: 50
  timeout: 120

# Optional
extends: "https://github.com/org/base-agent.git"
skills: [code-review, deploy]
delegation:
  mode: auto
compliance:
  risk_level: medium
  human_in_the_loop: true
```

## Tools

### Built-in Tools

| Tool | Description |
|---|---|
| `cli` | Execute shell commands |
| `read` | Read files with pagination |
| `write` | Write/create files |
| `memory` | Load/save git-committed memory |

### Declarative Tools

Define tools as YAML in `tools/`:

```yaml
# tools/search.yaml
name: search
description: Search the codebase
input_schema:
  properties:
    query:
      type: string
      description: Search query
    path:
      type: string
      description: Directory to search
  required: [query]
implementation:
  script: search.sh
  runtime: sh
```

The script receives args as JSON on stdin and returns output on stdout.

## Hooks

Script-based hooks in `hooks/hooks.yaml`:

```yaml
hooks:
  on_session_start:
    - script: validate-env.sh
      description: Check environment is ready
  pre_tool_use:
    - script: audit-tools.sh
      description: Log and gate tool usage
  post_response:
    - script: notify.sh
  on_error:
    - script: alert.sh
```

Hook scripts receive context as JSON on stdin and return:

```json
{ "action": "allow" }
{ "action": "block", "reason": "Not permitted" }
{ "action": "modify", "args": { "modified": "args" } }
```

## Skills

Skills are composable instruction modules in `skills/<name>/`:

```
skills/
  code-review/
    SKILL.md
    scripts/
      lint.sh
```

```markdown
---
name: code-review
description: Review code for quality and security
---

# Code Review

When reviewing code:
1. Check for security vulnerabilities
2. Verify error handling
3. Run the lint script for style checks
```

Invoke via CLI: `/skill:code-review Review the auth module`

## Plugins

Plugins are reusable extensions that can provide tools, hooks, skills, prompts, and memory layers. They follow the same git-native philosophy — a plugin is a directory with a `plugin.yaml` manifest.

### CLI Commands

```bash
# Install from git URL
gitclaw plugin install https://github.com/org/my-plugin.git

# Install from local path
gitclaw plugin install ./path/to/plugin

# Install with options
gitclaw plugin install <source> --name custom-name --force --no-enable

# List all discovered plugins
gitclaw plugin list

# Enable / disable
gitclaw plugin enable my-plugin
gitclaw plugin disable my-plugin

# Remove
gitclaw plugin remove my-plugin

# Scaffold a new plugin
gitclaw plugin init my-plugin
```

| Flag | Description |
|---|---|
| `--name <name>` | Custom plugin name (default: derived from source) |
| `--force` | Reinstall even if already present |
| `--no-enable` | Install without auto-enabling |

### Plugin Manifest (`plugin.yaml`)

```yaml
id: my-plugin                    # Required, kebab-case
name: My Plugin
version: 0.1.0
description: What this plugin does
author: Your Name
license: MIT
engine: ">=0.3.0"               # Min gitclaw version

provides:
  tools: true                    # Load tools from tools/*.yaml
  skills: true                   # Load skills from skills/
  prompt: prompt.md              # Inject into system prompt
  hooks:
    pre_tool_use:
      - script: hooks/audit.sh
        description: Audit tool calls

config:
  properties:
    api_key:
      type: string
      description: API key
      env: MY_API_KEY            # Env var fallback
    timeout:
      type: number
      default: 30
  required: [api_key]

entry: index.ts                  # Optional programmatic entry point
```

### Plugin Config in `agent.yaml`

```yaml
plugins:
  my-plugin:
    enabled: true
    source: https://github.com/org/my-plugin.git  # Auto-install on load
    version: main                                   # Git branch/tag
    config:
      api_key: "${MY_API_KEY}"                      # Supports env interpolation
      timeout: 60
```

Config resolution priority: `agent.yaml config` > `env var` > `manifest default`.

### Discovery Order

Plugins are discovered in this order (first match wins):

1. **Local** — `<agent-dir>/plugins/<name>/`
2. **Global** — `~/.gitclaw/plugins/<name>/`
3. **Installed** — `<agent-dir>/.gitagent/plugins/<name>/`

### Programmatic Plugins

Plugins with an `entry` field in their manifest get a full API:

```typescript
// index.ts
import type { GitclawPluginApi } from "gitclaw";

export async function register(api: GitclawPluginApi) {
  // Register a tool
  api.registerTool({
    name: "search_docs",
    description: "Search documentation",
    inputSchema: {
      properties: { query: { type: "string" } },
      required: ["query"],
    },
    handler: async (args) => {
      const results = await search(args.query);
      return { text: JSON.stringify(results) };
    },
  });

  // Register a lifecycle hook
  api.registerHook("pre_tool_use", async (ctx) => {
    api.logger.info(`Tool called: ${ctx.tool}`);
    return { action: "allow" };
  });

  // Add to system prompt
  api.addPrompt("Always check docs before answering questions.");

  // Register a memory layer
  api.registerMemoryLayer({
    name: "docs-cache",
    path: "memory/docs-cache.md",
    description: "Cached documentation lookups",
  });
}
```

**Available API methods:**

| Method | Description |
|---|---|
| `registerTool(def)` | Register a tool the agent can call |
| `registerHook(event, handler)` | Register a lifecycle hook (`on_session_start`, `pre_tool_use`, `post_response`, `on_error`) |
| `addPrompt(text)` | Append text to the system prompt |
| `registerMemoryLayer(layer)` | Register a memory layer |
| `logger.info/warn/error(msg)` | Prefixed logging (`[plugin:id]`) |
| `pluginId` | Plugin identifier |
| `pluginDir` | Absolute path to plugin directory |
| `config` | Resolved config values |

### Plugin Structure

```
my-plugin/
├── plugin.yaml          # Manifest (required)
├── tools/               # Declarative tool definitions
│   └── *.yaml
├── hooks/               # Hook scripts
├── skills/              # Skill modules
├── prompt.md            # System prompt addition
└── index.ts             # Programmatic entry point
```

## Multi-Model Support

Gitclaw works with any LLM provider supported by [pi-ai](https://github.com/nicepkg/pi-ai):

```yaml
# agent.yaml
model:
  preferred: "anthropic:claude-sonnet-4-5-20250929"
  fallback:
    - "openai:gpt-4o"
    - "google:gemini-2.0-flash"
```

Supported providers: `anthropic`, `openai`, `google`, `xai`, `groq`, `mistral`, and more.

## Inheritance & Composition

Agents can extend base agents:

```yaml
# agent.yaml
extends: "https://github.com/org/base-agent.git"

# Dependencies
dependencies:
  - name: shared-tools
    source: "https://github.com/org/shared-tools.git"
    version: main
    mount: tools

# Sub-agents
delegation:
  mode: auto
```

## Compliance & Audit

Built-in compliance validation and audit logging:

```yaml
# agent.yaml
compliance:
  risk_level: high
  human_in_the_loop: true
  data_classification: confidential
  regulatory_frameworks: [SOC2, GDPR]
  recordkeeping:
    audit_logging: true
    retention_days: 90
```

Audit logs are written to `.gitagent/audit.jsonl` with full tool invocation traces.

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](./CONTRIBUTING.md) for guidelines.

## License

This project is licensed under the [MIT License](./LICENSE).
