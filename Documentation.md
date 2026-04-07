# GitClaw Documentation

> **GitClaw** — A universal git-native multimodal always-learning AI Agent
> Version 1.3.3 | MIT License | [github.com/open-gitagent/gitclaw](https://github.com/open-gitagent/gitclaw)

---

## Table of Contents

- [Quick Start](#quick-start)
- [Installation](#installation)
- [CLI Reference](#cli-reference)
- [Agent Configuration (agent.yaml)](#agent-configuration)
- [Models & Providers](#models--providers)
- [Voice Mode](#voice-mode)
- [Web UI](#web-ui)
- [Built-in Tools](#built-in-tools)
- [Skills](#skills)
- [Workflows & SkillFlows](#workflows--skillflows)
- [Hooks](#hooks)
- [Plugins](#plugins)
- [Memory System](#memory-system)
- [Schedules & Cron](#schedules--cron)
- [Integrations](#integrations)
- [Compliance & Audit](#compliance--audit)
- [SDK (Programmatic Usage)](#sdk)
- [Context Compaction](#context-compaction)
- [Cost Tracking](#cost-tracking)
- [Security & Password Protection](#security--password-protection)
- [Directory Structure](#directory-structure)
- [Environment Variables](#environment-variables)

---

## Quick Start

```bash
# One-line install & launch
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitclaw/main/install.sh | bash
```

This installs GitClaw globally via npm, walks you through setup (API keys, voice adapter, model), and launches the web UI at `http://localhost:3333`.

---

## Installation

### Requirements

- **Node.js 20+** (required by WhatsApp dependency)
- **Git** (for memory commits and session branches)
- **npm** (included with Node.js)

### Install Methods

**Interactive installer (recommended):**
```bash
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitclaw/main/install.sh | bash
```

**Manual install:**
```bash
npm install -g gitclaw
mkdir ~/assistant && cd ~/assistant && git init
gitclaw --voice --dir .
```

### Setup Modes

The installer offers four options:

| Mode | Description | Keys Required |
|------|-------------|---------------|
| **Install with LYZR** | Easiest — uses Lyzr AI Studio cloud | `LYZR_API_KEY` |
| **Voice + Text** | Real-time voice + text chat | `OPENAI_API_KEY` + `ANTHROPIC_API_KEY` |
| **Text Only** | Browser text chat, no voice | `ANTHROPIC_API_KEY` |
| **Advanced Setup** | Choose voice adapter, model, port, integrations | varies |

### Updating

```bash
# The installer auto-detects existing installations and offers to update
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitclaw/main/install.sh | bash

# Or manually
npm update -g gitclaw
```

---

## CLI Reference

### Basic Usage

```bash
# Launch voice/web UI
gitclaw --voice --dir ~/assistant

# Single-shot query (no REPL)
gitclaw --dir ~/assistant "Build a REST API for user management"

# Interactive REPL
gitclaw --dir ~/assistant

# With specific model
gitclaw --model anthropic:claude-opus-4-6 --voice --dir ~/assistant
```

### Flags

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--model` | `-m` | Model to use (`provider:model-id`) | from agent.yaml |
| `--dir` | `-d` | Agent directory | current directory |
| `--prompt` | `-p` | Single-shot prompt | — |
| `--env` | `-e` | Environment config (loads `config/<env>.yaml`) | default |
| `--voice` | `-v` | Enable voice mode (optionally: `openai` or `gemini`) | — |
| `--sandbox` | `-s` | Run in E2B sandbox VM | false |
| `--sandbox-repo` | — | Repository URL for sandbox | — |
| `--sandbox-token` | — | E2B API token | `E2B_API_KEY` env |
| `--repo` | `-r` | Clone and work on remote repository | — |
| `--pat` | — | GitHub/GitLab personal access token | `GITHUB_TOKEN` env |
| `--session` | — | Git branch name for session isolation | auto-generated |

### REPL Commands

| Command | Description |
|---------|-------------|
| `/quit` or `/exit` | Exit the session |
| `/memory` | View the memory file |
| `/skills` | List installed skills |
| `/tasks` | Show active learning tasks |
| `/learned` | List learned skills with confidence scores |
| `/plugins` | List loaded plugins |
| `/skill:name args` | Invoke a specific skill |

### Plugin CLI

```bash
gitclaw plugin install https://github.com/user/plugin-repo
gitclaw plugin install ./local/path --name my-plugin --force
gitclaw plugin list --dir ~/assistant
gitclaw plugin enable my-plugin --dir ~/assistant
gitclaw plugin disable my-plugin --dir ~/assistant
gitclaw plugin remove my-plugin --dir ~/assistant
gitclaw plugin init my-plugin --dir ~/assistant
```

---

## Agent Configuration

GitClaw agents are configured via `agent.yaml` in the agent directory.

### Full Schema

```yaml
spec_version: "0.1.0"
name: my-agent
version: 1.0.0
description: A description of what this agent does

model:
  preferred: "anthropic:claude-sonnet-4-6"
  fallback:
    - "openai:gpt-4o"
    - "google:gemini-2.0-flash-001"
  constraints:
    temperature: 0.7
    max_tokens: 4096
    top_p: 0.9
    top_k: 40
    stop_sequences: ["---"]

tools:
  - cli
  - read
  - write
  - memory
  - capture_photo
  - task_tracker
  - skill_learner

skills:
  - code-review
  - deployment

runtime:
  max_turns: 50
  timeout: 300  # seconds per tool call

# Inheritance (optional)
extends: https://github.com/user/parent-agent.git

# Dependencies (optional)
dependencies:
  - name: shared-skills
    source: https://github.com/team/shared-skills
    version: main
    mount: deps/shared

# Sub-agents (optional)
agents:
  researcher:
    model: "anthropic:claude-haiku-4-5-20251001"
    tools: [read, cli]

delegation:
  mode: auto  # auto | explicit | router

# Plugins (optional)
plugins:
  my-plugin:
    enabled: true
    config:
      api_key: "${MY_PLUGIN_KEY}"

# Compliance (optional — for enterprise)
compliance:
  risk_level: high
  human_in_the_loop: true
  data_classification: "confidential"
  regulatory_frameworks: [SOX, GLBA]
  recordkeeping:
    audit_logging: true
    retention_days: 2555
  review:
    required_approvers: 2
    auto_review: false

# Serve mode (optional)
serve:
  port: 8080
  allowed_tools: [lookup_account, get_policy]
  constraints:
    temperature: 0
    max_tokens: 4000
```

### Model Resolution Order

1. Environment config `model_override` (from `config/<env>.yaml`)
2. CLI flag `--model provider:model-id`
3. `agent.yaml` `model.preferred`

### Identity Files

| File | Purpose |
|------|---------|
| `SOUL.md` | Agent personality, identity, core values |
| `RULES.md` | Behavioral constraints and rules |
| `DUTIES.md` | Job responsibilities and tasks |
| `AGENTS.md` | Sub-agent relationships and delegation rules |

---

## Models & Providers

### Supported Providers

GitClaw supports any model from the following providers out of the box:

| Provider | Format | API Key Env Var |
|----------|--------|-----------------|
| Anthropic | `anthropic:claude-sonnet-4-6` | `ANTHROPIC_API_KEY` |
| OpenAI | `openai:gpt-4o` | `OPENAI_API_KEY` |
| Google | `google:gemini-2.0-flash-001` | `GEMINI_API_KEY` |
| Groq | `groq:llama-3.3-70b-versatile` | `GROQ_API_KEY` |
| xAI | `xai:grok-2-1212` | `XAI_API_KEY` |
| Mistral | `mistral:mistral-large-latest` | `MISTRAL_API_KEY` |
| OpenRouter | `openrouter:anthropic/claude-3.5-sonnet` | `OPENROUTER_API_KEY` |
| Cerebras | `cerebras:llama3.1-70b` | `CEREBRAS_API_KEY` |
| DeepSeek | `deepseek:deepseek-chat` | `DEEPSEEK_API_KEY` |
| Amazon Bedrock | `amazon-bedrock:anthropic.claude-3-sonnet` | AWS credentials |
| Google Vertex | `google-vertex:gemini-2.5-flash` | GCP ADC |
| Azure OpenAI | `azure-openai-responses:gpt-4o` | `AZURE_OPENAI_API_KEY` |

### Custom / OpenAI-Compatible Endpoints

Any endpoint that implements the OpenAI Chat Completions API:

**Inline URL:**
```bash
gitclaw --model "ollama:llama3@http://localhost:11434/v1" --voice --dir ~/assistant
```

**Environment variable:**
```bash
export GITCLAW_MODEL_BASE_URL=http://localhost:11434/v1
gitclaw --model "ollama:llama3" --voice --dir ~/assistant
```

**In agent.yaml:**
```yaml
model:
  preferred: "custom:my-model@https://my-proxy.com/v1"
```

**Supported custom endpoints:**
- Ollama (`http://localhost:11434/v1`)
- LM Studio (`http://localhost:1234/v1`)
- vLLM (`http://localhost:8000/v1`)
- LiteLLM (`http://localhost:4000/v1`)
- Lyzr AI Studio (`https://agent-prod.studio.lyzr.ai/v4/chat`)
- Any OpenAI-compatible proxy

### Lyzr Integration

GitClaw integrates with [Lyzr AI Studio](https://studio.lyzr.ai) as an agent brain. The Lyzr completions endpoint is fully OpenAI-compatible.

**Via installer (easiest):**
```bash
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitclaw/main/install.sh | bash
# Pick option 1: "Install with LYZR"
# Enter your Lyzr API key — agent is created automatically
```

**Via CLI flag:**
```bash
export OPENAI_API_KEY="your-lyzr-api-key"   # Lyzr uses standard Bearer auth
gitclaw --model "lyzr:<agent-id>@https://agent-prod.studio.lyzr.ai/v4" --voice --dir ~/assistant
```

**Via SDK (programmatic):**
```typescript
import { query } from "gitclaw";

// Set OPENAI_API_KEY to your Lyzr API key (uses standard Bearer auth)
process.env.OPENAI_API_KEY = process.env.LYZR_API_KEY;

const result = query({
  prompt: "Hello! What can you help me with?",
  dir: "/path/to/agent",
  model: `lyzr:${LYZR_AGENT_ID}@https://agent-prod.studio.lyzr.ai/v4`,
  constraints: { temperature: 0.7, maxTokens: 2000 },
});

for await (const msg of result) {
  if (msg.type === "assistant") console.log(msg.content);
}
```

**How it works:**
- Base URL: `https://agent-prod.studio.lyzr.ai/v4` (OpenAI SDK appends `/chat/completions`)
- Auth: `Authorization: Bearer <LYZR_API_KEY>` (standard OpenAI-compatible)
- Model field: your Lyzr agent ID (e.g., `69d52b90a011dc91d7877bfd`)
- Full example: `examples/lyzr-sdk.ts`

---

## Voice Mode

GitClaw supports real-time bidirectional voice via two adapters:

### OpenAI Realtime (default)

- Model: `gpt-realtime-2025-08-28`
- Real-time audio streaming over WebSocket
- Supports image input (camera frames)
- Requires: `OPENAI_API_KEY`

### Gemini Live

- Model: `gemini-2.0-flash`
- Alternative voice provider
- Free tier available
- Requires: `GEMINI_API_KEY`

```bash
# OpenAI voice (default)
gitclaw --voice --dir ~/assistant

# Gemini voice
gitclaw --voice gemini --dir ~/assistant
```

### Text-Only Fallback

If no voice API key is set, GitClaw still starts the web UI server but with voice disabled. A warning banner appears in the UI, mic/camera/speaker buttons are hidden, and text input routes directly to the agent via `query()`.

### Camera

- Front/back camera toggle (mobile)
- Captures frames every 1 second as JPEG
- Frames injected into conversation as images
- Auto-captures on "memorable moments" (laughter, excitement)

---

## Web UI

The voice server runs at `http://localhost:3333` and provides a full-featured web interface.

### Tabs

| Tab | Features |
|-----|----------|
| **Chat** | Real-time conversation, voice controls, camera, agent vitals, file system viewer |
| **Skills** | Browse and install skills from the marketplace |
| **Integrations** | Connect Composio services (Gmail, Calendar, Slack, GitHub) |
| **Communication** | Telegram bot setup, WhatsApp connection, phone/SMS webhook |
| **SkillFlows** | Visual workflow builder — chain skills into multi-step flows |
| **Scheduler** | Create cron jobs — run prompts on a schedule |
| **Settings** | Model selection, API keys, custom base URL — saves to `.env` and `agent.yaml` |

### Agent Vitals

Real-time metrics displayed in the Chat tab:
- **CPU** — Delta-based percentage (blue)
- **Memory** — RSS in MB (orange)
- **Tokens** — Total tokens used in session (purple)
- **Uptime** — Server uptime synced from backend (green)
- **Pulse** — CPU wave visualization

### Mobile Responsive

The UI is responsive under 700px:
- Tabs become a scrollable horizontal strip
- Camera panel stacks vertically
- Controls have 44px touch targets
- Sidebar overlays instead of pushing content
- All views stack vertically

---

## Built-in Tools

| Tool | Description | Concurrency Safe | Read Only |
|------|-------------|-----------------|-----------|
| `cli` | Run shell commands | No | No |
| `read` | Read file contents | Yes | Yes |
| `write` | Create/write files | No | No |
| `memory` | Load/save persistent memory | No | No |
| `capture_photo` | Capture camera frame as photo | No | No |
| `task_tracker` | Track task progress, search skills | No | No |
| `skill_learner` | Save/evaluate learned skills | No | No |

### CLI Tool

```
Command: ls -la src/
Timeout: 120s (configurable)
Output: stdout + stderr (truncated to ~100KB)
```

### Read Tool

```
Path: src/index.ts
Encoding: utf-8 (default) or base64
Partial reads: start/end byte offsets
```

### Write Tool

```
Path: workspace/report.md
Content: "# Report\n..."
Append: false (default) — overwrites
Auto-creates parent directories
```

### Memory Tool

- **load** — Returns current `memory/MEMORY.md` content
- **save** — Appends entry + git commits
- Supports layered memory via `memory.yaml`
- Auto-archives when `max_lines` exceeded (to `memory/archive/<YYYY-MM>.md`)

### Declarative Tools (Custom)

Define tools in `tools/*.yaml`:

```yaml
name: lookup-account
description: Look up account details by customer ID
input_schema:
  properties:
    customer_id:
      type: string
      description: The customer ID
  required: [customer_id]
implementation:
  script: scripts/lookup.sh
  runtime: sh
```

The script receives JSON args on stdin and outputs plain text.

---

## Skills

Skills are reusable instruction sets that the agent follows for specific tasks.

### Creating a Skill

Create `skills/<skill-name>/SKILL.md`:

```markdown
---
name: code-review
description: Review code for bugs, style, and security issues
license: MIT
allowed-tools: cli read write
metadata:
  author: your-name
  version: "1.0.0"
  category: development
---

# Code Review

## Instructions

1. Read the specified file(s) using the read tool
2. Analyze for:
   - Bugs and logic errors
   - Security vulnerabilities (OWASP top 10)
   - Code style and readability
   - Performance issues
3. Write a review report to workspace/review.md

## Output Format

For each issue found:
- **File**: path
- **Line**: number
- **Severity**: critical / warning / info
- **Description**: what's wrong
- **Fix**: suggested change
```

### Invoking Skills

```bash
# In REPL
/skill:code-review Review the auth module

# In voice/text
"Use the code-review skill on src/auth.ts"
```

### Skill Learning

The agent can learn new skills automatically:

1. `task_tracker` begins tracking a task
2. Agent completes the task successfully
3. `skill_learner` evaluates if the approach is worth saving
4. If yes, crystallizes it as a new skill with `confidence: 0.7`
5. Future tasks search for matching skills
6. Confidence adjusts based on success/failure outcomes

---

## Workflows & SkillFlows

### Basic Workflow (reference)

`workflows/cleanup.md`:
```markdown
---
name: cleanup
description: Clean up temporary files
---

# Cleanup Workflow
Remove temp files and rebuild.
```

### SkillFlow (executable multi-step)

`workflows/data-pipeline.yaml`:
```yaml
name: data-pipeline
description: Process data through validation, transformation, and storage
steps:
  - skill: validate-input
    prompt: "Validate the CSV data format"

  - skill: __approval_gate__
    prompt: "Data validation complete. Approve to continue?"
    channel: telegram

  - skill: transform-data
    prompt: "Transform to required schema"

  - skill: save-to-database
    prompt: "Store results"
```

### Approval Gates

Steps with `skill: __approval_gate__` pause execution and send an approval request via the specified channel (Telegram, WhatsApp). The user has 5 minutes to approve before timeout.

---

## Hooks

Hooks intercept agent lifecycle events for validation, logging, and control.

### Configuration

`hooks/hooks.yaml`:
```yaml
hooks:
  on_session_start:
    - script: hooks/check-auth.sh
      description: "Verify user authorization"

  pre_tool_use:
    - script: hooks/validate-command.sh
      description: "Block dangerous CLI commands"

  post_tool_failure:
    - script: hooks/notify-error.sh

  post_response:
    - script: hooks/log-response.sh

  pre_query:
    - script: hooks/rate-limit.sh

  file_changed:
    - script: hooks/track-changes.sh

  on_error:
    - script: hooks/incident-report.sh
```

### Hook Events

| Event | When | Can Block | Can Modify Args |
|-------|------|-----------|----------------|
| `on_session_start` | Before agent runs | Yes | No |
| `pre_tool_use` | Before each tool call | Yes | Yes |
| `post_tool_failure` | After a tool errors | No | No |
| `pre_query` | Before LLM call | Yes | No |
| `post_response` | After LLM responds | No | No |
| `file_changed` | After file write | No | No |
| `on_error` | On agent error | No | No |

### Hook Script Format

Scripts receive JSON on stdin and output JSON on stdout:

**Input:**
```json
{"event": "pre_tool_use", "session_id": "uuid", "tool": "cli", "args": {"command": "rm -rf /"}}
```

**Output:**
```json
{"action": "block", "reason": "Destructive command blocked"}
```

**Actions:** `allow`, `block`, `modify` (with `args` field for modified arguments)

### Programmatic Hooks (SDK)

```typescript
const result = query({
  hooks: {
    preToolUse: async (ctx) => {
      if (ctx.toolName === "cli" && ctx.args.command.includes("rm")) {
        return { action: "block", reason: "Blocked rm command" };
      }
      return { action: "allow" };
    },
  },
});
```

---

## Plugins

Plugins extend GitClaw with tools, skills, hooks, memory layers, and prompt additions.

### Plugin Manifest

`plugins/my-plugin/plugin.yaml`:
```yaml
id: my-plugin
name: My Plugin
version: 1.0.0
description: What this plugin does
author: Your Name
license: MIT
engine: ">=1.0.0"

provides:
  tools: true
  skills: true
  prompt: prompt.md
  hooks:
    pre_tool_use:
      - script: hooks/validate.sh

memory:
  - name: plugin-data
    path: memory/plugin-data.md
    max_lines: 500
```

### Plugin Structure

```
plugins/my-plugin/
  plugin.yaml          # manifest
  prompt.md            # appended to system prompt
  tools/
    my-tool.yaml       # declarative tools
  skills/
    my-skill/
      SKILL.md
  hooks/
    validate.sh
```

### Plugin Management

```bash
gitclaw plugin install https://github.com/user/plugin
gitclaw plugin list
gitclaw plugin remove my-plugin
gitclaw plugin init my-plugin  # scaffold a new plugin
```

---

## Memory System

GitClaw's memory is git-native — all memory changes are committed, versioned, and auditable.

### Memory File

`memory/MEMORY.md` — the primary memory file, loaded into every conversation.

### Memory Layers

Configure in `memory/memory.yaml`:
```yaml
layers:
  - name: main
    path: memory/MEMORY.md
    max_lines: 200
  - name: technical
    path: memory/technical.md
    max_lines: 100
```

### Auto-Archiving

When a layer exceeds `max_lines`, old entries are moved to `memory/archive/<YYYY-MM>.md`.

### Additional Memory Features

| Feature | Location | Description |
|---------|----------|-------------|
| **Mood log** | `memory/mood.md` | Session mood tracking (happy, frustrated, curious, excited, calm) |
| **Photos** | `memory/photos/` | Captured memorable moments with INDEX.md |
| **Journal** | `memory/journal/<date>.md` | Auto-generated session reflections |
| **Learning** | `.gitagent/learning/` | Task history and learned skills (JSON) |

### Memory Detection

The agent automatically detects and saves personal information from voice transcripts:
- Names, preferences, locations
- Job titles, responsibilities
- Important dates, relationships

---

## Schedules & Cron

Schedule recurring or one-time tasks.

### Schedule Definition

`schedules/daily-standup.yaml`:
```yaml
id: daily-standup
prompt: "Summarize git commits from the last 24 hours and list open tasks"
cron: "0 9 * * 1-5"
mode: repeat
enabled: true
```

### One-Time Schedule

```yaml
id: quarterly-review
prompt: "Generate Q1 performance report"
mode: once
runAt: "2026-04-01T09:00:00Z"
enabled: true
```

### Cron Patterns

| Pattern | Meaning |
|---------|---------|
| `0 9 * * 1-5` | Weekdays at 9 AM |
| `0 9 * * 1` | Every Monday at 9 AM |
| `0 9 1 * *` | First of month at 9 AM |
| `0 9 1 */3 *` | Quarterly |
| `*/30 * * * *` | Every 30 minutes |

### Managing via UI

The **Scheduler** tab in the web UI lets you create, edit, enable/disable, trigger, and delete schedules.

---

## Integrations

### Composio (Gmail, Calendar, Slack, GitHub)

Requires: `COMPOSIO_API_KEY`

Enables 200+ integrations via Composio's toolkit system. Configure in the **Integrations** tab.

### Telegram

Requires: `TELEGRAM_BOT_TOKEN`

- Create a bot via [@BotFather](https://t.me/botfather)
- Enter token in the **Communication** tab or during setup
- Configure allowed users for access control
- Files generated by the agent are auto-sent to Telegram

### WhatsApp

Uses the Baileys library (no phone number API needed):
- Connect via QR code in the **Communication** tab
- Session persists across restarts
- Auto-responds to messages from your number

### Phone / SMS (Twilio)

Configure a Twilio webhook pointing to:
```
https://your-server:3333/api/phone/webhook
```

---

## Compliance & Audit

### Compliance Configuration

In `agent.yaml`:
```yaml
compliance:
  risk_level: critical          # low | medium | high | critical
  human_in_the_loop: true
  data_classification: "PCI-DSS"
  regulatory_frameworks: [SOX, GLBA, OCC]
  recordkeeping:
    audit_logging: true
    retention_days: 2555        # 7 years for banking
  review:
    required_approvers: 2
    auto_review: false
```

### Validation Rules

| Rule | Condition | Severity |
|------|-----------|----------|
| `high_risk_hitl` | High/critical risk without `human_in_the_loop` | warning |
| `critical_audit` | Critical risk without `audit_logging` | **error (blocks startup)** |
| `regulatory_recordkeeping` | Regulatory frameworks without recordkeeping | warning |
| `high_risk_review` | High/critical risk without review config | warning |
| `audit_retention` | Audit logging without `retention_days` | warning |

### Audit Log

When `audit_logging: true`, all actions are logged to `.gitagent/audit.jsonl`:

```json
{"timestamp":"2026-01-15T14:23:45Z","session_id":"uuid","event":"session_start"}
{"timestamp":"2026-01-15T14:23:46Z","session_id":"uuid","event":"tool_use","tool":"cli","args":{"command":"ls"}}
{"timestamp":"2026-01-15T14:23:47Z","session_id":"uuid","event":"tool_result","tool":"cli","result":"file.txt"}
{"timestamp":"2026-01-15T14:23:48Z","session_id":"uuid","event":"response"}
{"timestamp":"2026-01-15T14:23:49Z","session_id":"uuid","event":"session_end"}
```

---

## SDK

GitClaw can be used programmatically as an npm package.

### Installation

```bash
npm install gitclaw
```

### Basic Usage

```typescript
import { query } from "gitclaw";

const result = query({
  prompt: "Create a Python script that sorts a CSV file by the 'date' column",
  dir: "/path/to/agent",
});

for await (const msg of result) {
  if (msg.type === "assistant") {
    console.log(msg.content);
  }
  if (msg.type === "tool_use") {
    console.log(`Using tool: ${msg.toolName}`);
  }
}

// Get cost breakdown
console.log(result.costs());
```

### Custom Tools

```typescript
import { query, tool } from "gitclaw";

const weatherTool = tool(
  "get_weather",
  "Get current weather for a city",
  { properties: { city: { type: "string" } }, required: ["city"] },
  async (args) => {
    const res = await fetch(`https://api.weather.com/${args.city}`);
    return await res.text();
  }
);

const result = query({
  prompt: "What's the weather in Tokyo?",
  dir: "/path/to/agent",
  tools: [weatherTool],
});
```

### buildTool Factory

```typescript
import { buildTool } from "gitclaw";

const myTool = buildTool({
  name: "search_docs",
  description: "Search documentation",
  parameters: { properties: { query: { type: "string" } }, required: ["query"] },
  execute: async (args) => {
    // ... search logic
    return "Results: ...";
  },
  metadata: {
    isConcurrencySafe: true,   // safe to run in parallel
    isReadOnly: true,           // no side effects
    maxResultSizeChars: 20000,  // truncate large results
  },
});
```

### Hooks

```typescript
const result = query({
  prompt: "Deploy to production",
  dir: "/path/to/agent",
  hooks: {
    onSessionStart: async (ctx) => ({ action: "allow" }),
    preToolUse: async (ctx) => {
      if (ctx.toolName === "cli" && ctx.args.command.includes("deploy")) {
        console.log("Deployment detected — requiring approval");
        return { action: "block", reason: "Manual approval required" };
      }
      return { action: "allow" };
    },
    postResponse: async (ctx) => {
      console.log(`Session ${ctx.sessionId} responded`);
    },
    onError: async (ctx) => {
      console.error(`Error in session ${ctx.sessionId}: ${ctx.error}`);
    },
  },
});
```

### Query Options

```typescript
query({
  prompt: "...",                          // string or AsyncIterable<GCUserMessage>
  dir: "/path/to/agent",                 // agent directory
  model: "anthropic:claude-opus-4-6",    // override model
  env: "production",                      // load config/production.yaml
  systemPrompt: "Custom prompt...",       // replace system prompt
  systemPromptSuffix: "Extra context...", // append to system prompt
  tools: [myTool],                        // inject custom tools
  replaceBuiltinTools: true,              // disable built-in tools
  allowedTools: ["read", "write"],        // whitelist
  disallowedTools: ["cli"],               // blacklist
  maxTurns: 10,                           // limit agent turns
  constraints: { temperature: 0 },        // model constraints
  sessionId: "custom-id",                 // custom session ID
  abortController: new AbortController(), // cancel execution
});
```

---

## Context Compaction

Utilities for managing context window limits in long conversations.

```typescript
import { estimateTokens, estimateMessageTokens, needsCompaction, truncateToolResults, buildCompactPrompt } from "gitclaw";

// Estimate tokens
const tokens = estimateTokens("Hello world");  // ~3

// Check if compaction needed (triggers at 75% of context window)
const { needed, ratio } = needsCompaction(messages, 200000);

// Truncate oversized tool results (keeps first + last half)
const trimmed = truncateToolResults(messages, 10000);

// Build a summarization prompt
const prompt = buildCompactPrompt(messages);
```

---

## Cost Tracking

Track token usage and costs per model across sessions.

```typescript
import { CostTracker } from "gitclaw";

const tracker = new CostTracker();

// Automatically tracked when using query()
const result = query({ prompt: "...", dir: "..." });
for await (const msg of result) { /* ... */ }

const costs = result.costs();
// {
//   totalCostUsd: 0.05,
//   totalInputTokens: 5000,
//   totalOutputTokens: 2000,
//   totalRequests: 3,
//   modelUsage: {
//     "anthropic:claude-sonnet-4-6": { inputTokens: 5000, ... }
//   }
// }
```

---

## Security & Password Protection

### Password Protection

Set `GITCLAW_PASSWORD` to require authentication for the web UI:

```bash
GITCLAW_PASSWORD=mysecret gitclaw --voice --dir ~/assistant
```

When set:
- All HTTP routes show a login page instead of the UI
- WebSocket connections are rejected without valid auth cookie
- `/health` endpoint remains open (for load balancers)
- Cookie: `HttpOnly`, `SameSite=Strict`, 24-hour expiry
- Token is SHA-256 hash (password never stored in cookie)

### Best Practices

- Use HTTPS in production (via nginx, Caddy, or Cloudflare Tunnel)
- Set `GITCLAW_PASSWORD` when exposing to a network
- Use OpenShell for kernel-level sandboxing in enterprise deployments
- Enable audit logging for compliance (`compliance.recordkeeping.audit_logging: true`)

---

## Directory Structure

```
~/assistant/                          # Agent root (git repo)
├── agent.yaml                        # Agent manifest
├── SOUL.md                           # Agent identity
├── RULES.md                          # Behavior rules (optional)
├── DUTIES.md                         # Responsibilities (optional)
├── .env                              # API keys (gitignored)
├── .gitignore
│
├── workspace/                        # Output directory
│
├── memory/                           # Persistent memory
│   ├── MEMORY.md                     # Main memory
│   ├── mood.md                       # Mood tracking
│   ├── photos/                       # Captured moments
│   │   └── INDEX.md
│   ├── journal/                      # Session reflections
│   └── archive/                      # Archived entries
│
├── skills/                           # Installed skills
│   └── skill-name/
│       └── SKILL.md
│
├── workflows/                        # SkillFlows
│   └── pipeline.yaml
│
├── schedules/                        # Cron jobs
│   └── daily-standup.yaml
│
├── hooks/                            # Lifecycle hooks
│   ├── hooks.yaml
│   └── validate.sh
│
├── tools/                            # Custom declarative tools
│   └── my-tool.yaml
│
├── plugins/                          # Installed plugins
│   └── plugin-id/
│       └── plugin.yaml
│
├── config/                           # Environment configs
│   ├── default.yaml
│   └── production.yaml
│
├── knowledge/                        # Knowledge base
│   └── domain.md
│
├── compliance/                       # Compliance config
│   ├── regulatory-map.yaml
│   └── validation-schedule.yaml
│
└── .gitagent/                        # Internal state (gitignored)
    ├── state.json
    ├── audit.jsonl
    └── learning/
        ├── tasks.json
        └── skills.json
```

---

## Environment Variables

| Variable | Description | Required |
|----------|-------------|----------|
| `OPENAI_API_KEY` | OpenAI API key (voice mode) | For voice |
| `ANTHROPIC_API_KEY` | Anthropic API key (agent brain) | For Anthropic models |
| `GEMINI_API_KEY` | Google Gemini key | For Gemini voice/models |
| `COMPOSIO_API_KEY` | Composio integrations | Optional |
| `TELEGRAM_BOT_TOKEN` | Telegram bot token | Optional |
| `LYZR_API_KEY` | Lyzr AI Studio key | For Lyzr setup |
| `GITCLAW_LYZR_AGENT_ID` | Lyzr agent ID (auto-created) | For Lyzr setup |
| `GITCLAW_MODEL_BASE_URL` | Custom LLM endpoint URL | Optional |
| `GITCLAW_PASSWORD` | Password-protect the web UI | Optional |
| `GITCLAW_ENV` | Environment name (loads config/<env>.yaml) | Optional |
| `GROQ_API_KEY` | Groq API key | For Groq models |
| `XAI_API_KEY` | xAI/Grok key | For xAI models |
| `MISTRAL_API_KEY` | Mistral key | For Mistral models |
| `OPENROUTER_API_KEY` | OpenRouter key | For OpenRouter |
| `DEEPSEEK_API_KEY` | DeepSeek key | For DeepSeek models |
| `E2B_API_KEY` | E2B sandbox key | For sandbox mode |
| `GITHUB_TOKEN` | GitHub PAT | For --repo mode |

---

*Built with love by the GitClaw team. MIT License.*
