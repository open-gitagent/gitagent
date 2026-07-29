# GitAgent Developer Training Session
### First Source Dev Team Onboarding
**Duration:** ~2 hours | **Format:** Live walkthrough + hands-on exercise

---

> **Facilitator note:** This document is your script, demo guide, and reference sheet in one. Each section includes talking points (what to say), demo steps (what to show), and key concepts to drive home. Code blocks are copy-paste ready for live demos.

---

## Pre-session Checklist

Before you start, make sure every participant has:

- [ ] Node.js 20+ installed (`node --version`)
- [ ] npm installed (`npm --version`)
- [ ] git installed (`git --version`)
- [ ] Terminal access (macOS/Linux/WSL)
- [ ] At least one API key ready: Anthropic, OpenAI, or a Lyzr Studio agent ID

---

## Section 1 — Introduction (5 min)

### What to say

"Most agent frameworks treat your AI configuration like application code — scattered across files, environment variables, and framework-specific APIs. GitAgent flips that completely.

In GitAgent, **your agent IS a git repository**. The personality is a markdown file. The rules are a markdown file. The memory is a markdown file that gets committed every time the agent remembers something. The tools, the skills, the hooks — all files in a repo you can clone, fork, branch, and diff.

Think about what that actually gives you. You can `git log` your agent's memory and see exactly how it evolved. You can `git diff` to see when a rule changed. You can branch off a 'strict-mode' version of your agent for production and a more experimental one for testing. You can fork a teammate's agent, inherit their entire personality and toolset, and customize from there. That's 'agents as repos' — and it's a fundamentally different mental model.

For a dev team like yours, this is powerful because you already know git. Every workflow you use for code — PRs, branch protection, CI checks — works exactly the same for your agents."

### Key points to emphasize

- The core insight: **the agent IS the git repo**, not code that describes an agent
- Git primitives (fork, diff, log, branch) become agent primitives
- No framework lock-in — configuration is plain text files

### What to show

Open a terminal and show the structure of a running GitAgent repo:

```
my-agent/
├── agent.yaml          # The manifest — model, tools, runtime config
├── SOUL.md             # Personality and identity
├── RULES.md            # Behavioral constraints
├── DUTIES.md           # Job responsibilities
├── AGENTS.md           # Sub-agent relationships
├── memory/
│   └── MEMORY.md       # Primary memory (auto-committed by the agent)
├── skills/
│   └── my-skill/
│       ├── SKILL.md    # Skill definition
│       └── scripts/    # Supporting scripts
├── hooks/
│   └── hooks.yaml      # Lifecycle hooks
└── tools/
    └── *.yaml          # Declarative tool definitions
```

"This is everything your agent needs to exist. Back it up, share it, version it, deploy it — it's just a directory."

---

## Section 2 — Installation & First Agent (15 min)

### What to say

"Let's get everyone running. There are two ways to install. The fastest is a one-command installer that handles everything interactively — API key setup, scaffolding, and launching the web UI."

### What to show

#### Option A: One-command install (recommended for today)

```bash
bash <(curl -fsSL "https://raw.githubusercontent.com/open-gitagent/gitagent/main/install.sh?$(date +%s)")
```

"That curl-bash will:
1. Install `@open-gitagent/gitagent` globally via npm (the slim CLI + SDK)
2. Install `@open-gitagent/voice` for the web UI at `localhost:3333`
3. Walk you through API key setup in interactive mode
4. Launch the web UI in your browser"

**Requirements:** Node.js 18+ (20+ recommended), npm, git

#### Option B: Manual install (for CI/sandboxed environments)

```bash
# Core CLI + SDK only (no voice, no web UI — good for headless/CI)
npm install -g @open-gitagent/gitagent

# Add voice mode + web UI
npm install -g @open-gitagent/voice
```

"If your security scanner flags the full install, use the slim core. It's about 85KB vs 180KB and has no third-party scanner triggers."

#### Scaffold your first agent

```bash
# Create a directory for your agent
mkdir ~/my-first-agent && cd ~/my-first-agent

# Run gitagent in it — it auto-scaffolds everything on first run
export OPENAI_API_KEY="sk-..."   # or ANTHROPIC_API_KEY, or LYZR_API_KEY
gitagent "Hello, what are you?"
```

"Watch what happens. GitAgent detects there's no `agent.yaml`, so it scaffolds one along with `SOUL.md`, `RULES.md`, and `memory/MEMORY.md` automatically. Then it answers your question."

#### Walk through what was created

```bash
ls -la ~/my-first-agent
cat ~/my-first-agent/agent.yaml
cat ~/my-first-agent/SOUL.md
cat ~/my-first-agent/RULES.md
cat ~/my-first-agent/memory/MEMORY.md
```

#### Launch the web UI

```bash
gitagent --voice   # Opens localhost:3333 in your browser
```

"The web UI has tabs for Chat, Skills, Integrations, Communication, SkillFlows, Scheduler, and Settings. We'll come back to several of these. For now, confirm everyone can open `localhost:3333`."

### Key points to emphasize

- Auto-scaffolding means zero manual setup to get started
- The slim install (`GITAGENT_SLIM=1`) skips voice for pipeline/CI use cases
- The web UI is optional — everything works headlessly too

---

## Section 3 — The #1 Question: Connecting to Lyzr Studio (10 min)

### What to say

"Before we go deeper into configuration, I want to address the question we get more than any other: 'How do I connect GitAgent to Lyzr Studio?' This is probably relevant to several of you, so let's do it now.

Lyzr Studio lets you build, manage, and orchestrate AI agents visually. GitAgent can use a Lyzr Studio agent as its model backend — meaning the intelligence comes from your Studio agent, and GitAgent provides the git-native structure, tools, memory, and hooks around it."

### What to show — step by step

#### Step 1: Get your LYZR_API_KEY

1. Go to [https://studio.lyzr.ai](https://studio.lyzr.ai) and log in
2. Navigate to **Settings → API Keys**
3. Copy your API key

```bash
export LYZR_API_KEY="lyzr-sk-..."
```

#### Step 2: Get your Agent ID

1. In Lyzr Studio, open the agent you want to connect
2. The agent ID is in the URL: `https://studio.lyzr.ai/agents/<agent-id>/...`
3. Or find it in the agent's **Settings** panel — it looks like `agent-abc123xyz`

#### Step 3: Set the model in agent.yaml

```yaml
# agent.yaml
spec_version: "0.1.0"
name: firstsource-agent
version: 0.1.0
description: First Source's GitAgent connected to Lyzr Studio

model:
  preferred: "lyzr:agent-abc123xyz@https://agent-prod.studio.lyzr.ai/v4"
  fallback:
    - "openai:gpt-4.1-mini"   # optional fallback if Studio is unreachable

tools:
  - cli
  - read
  - write
  - memory

runtime:
  max_turns: 40
```

The model string format is: `lyzr:<agent-id>@<studio-endpoint>`

#### Step 4: Run with explicit flags (if you prefer not to edit agent.yaml yet)

```bash
gitagent \
  --model "lyzr:agent-abc123xyz@https://agent-prod.studio.lyzr.ai/v4" \
  "Hello from GitAgent"
```

#### Step 5: Verify the connection

You should see the response come from your Lyzr Studio agent. Check the Studio dashboard — the agent's invocation count should increment.

### Common errors and fixes

| Error | Likely cause | Fix |
|---|---|---|
| `401 Unauthorized` | Wrong or missing API key | Check `echo $LYZR_API_KEY` is set correctly |
| `404 Not Found` | Wrong agent ID in the model string | Verify the agent ID from Studio URL |
| `Model provider not found: lyzr` | Outdated gitagent version | `npm install -g @open-gitagent/gitagent@latest` |
| Agent responds but ignores SOUL.md | Studio agent has its own system prompt | Either merge them in Studio, or use `systemPromptSuffix` in SDK |
| Timeout on first call | Studio agent cold start | Retry once; subsequent calls are faster |

### What to say (wrap-up)

"Once that's working, everything else we cover today — memory, skills, hooks — wraps around your Lyzr Studio agent. The Studio agent provides the intelligence; GitAgent provides the structure and control layer."

---

## Section 4 — Configuring Your Agent (15 min)

### What to say

"Now let's understand what you can actually configure. The starting point is always `agent.yaml` — it's the manifest that describes everything about your agent. But `agent.yaml` mostly wires things together. The real character of your agent lives in the identity files."

### What to show

#### agent.yaml — full reference example

```yaml
# agent.yaml
spec_version: "0.1.0"
name: firstsource-support-agent
version: 1.0.0
description: Customer support agent for First Source

# Model configuration
model:
  preferred: "anthropic:claude-sonnet-4-6"
  fallback:
    - "openai:gpt-4.1-mini"
    - "google:gemini-2.5-flash"

# Built-in tools to enable
tools:
  - cli
  - read
  - write
  - memory
  - task_tracker

# Runtime limits
runtime:
  max_turns: 40

# MCP servers (covered in Section 8)
mcp_servers:
  github:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_PERSONAL_ACCESS_TOKEN: "${GITHUB_PAT}"

# Compliance (covered in Section 9)
compliance:
  risk_level: medium
  human_in_the_loop: false
  audit_logging: true
  regulatory_frameworks: [SOC2]

# Sub-agents (optional — covered later)
agents:
  researcher:
    dir: "./agents/researcher"
    delegation:
      mode: explicit
```

**Multi-provider model strings:**

```yaml
# Anthropic
preferred: "anthropic:claude-sonnet-4-6"

# OpenAI
preferred: "openai:gpt-4.1-mini"

# Google
preferred: "google:gemini-2.5-flash"

# Groq (fast inference)
preferred: "groq:llama-3.3-70b-versatile"

# Local via Ollama
preferred: "ollama:llama3.2"

# Local via LM Studio
preferred: "lmstudio:mistral-7b"

# Lyzr Studio
preferred: "lyzr:agent-abc123@https://agent-prod.studio.lyzr.ai/v4"
```

#### SOUL.md — writing a good personality

"SOUL.md is your agent's personality file. It defines who the agent is — how it speaks, what it cares about, how it approaches problems. It becomes part of the system prompt on every query."

**What makes a good SOUL.md:**

```markdown
# Alex — First Source Support Agent

You are Alex, a senior customer support specialist at First Source Financial Services. You've been with the company for five years and you know the product inside out.

## How you work

- You respond concisely and directly. Support tickets aren't the place for preamble.
- You ask one clarifying question at a time — never a list of five questions at once.
- When you don't know something, you say so, then point to where the answer can be found.
- You use the ticket tracker to log every resolution step so teammates can pick up mid-thread.

## Tone

- Professional but human. You're not a bot — you're a specialist.
- Calm under pressure. Escalations don't fluster you.
- Never overpromise. If you say "I'll check on that," you check on it.

## Knowledge domain

You specialize in: account management, billing disputes, integration support, and API troubleshooting.
```

"Notice: no emoji, no corporate speak, concrete behaviors. Write it the way you'd brief a new hire on their first day."

#### RULES.md — behavioral constraints

"RULES.md is where you put hard constraints — things the agent must never do, always do, or require explicit approval for."

```markdown
# Rules

1. **Never share customer PII in responses.** Redact account numbers, SSNs, and contact details from any output visible to third parties.
2. **Read before modifying.** Always read a file before editing or overwriting it.
3. **Require approval for external API calls.** Any outbound HTTP request to a non-approved domain needs confirmation.
4. **No credentials in memory.** Never store API keys, tokens, or passwords in MEMORY.md.
5. **Escalate unresolved issues after 3 turns.** If a customer issue isn't resolved within three exchanges, create an escalation ticket and notify a human.
6. **Stay in scope.** Only operate within the current repository and approved external services.
```

#### DUTIES.md — job responsibilities

"DUTIES.md describes the agent's recurring responsibilities — what it's supposed to proactively do, what workflows it owns."

```markdown
# Duties

## Daily responsibilities
- Review open support tickets and triage by severity
- Check integration health dashboards and flag anomalies
- Update MEMORY.md with any new resolution patterns discovered

## On each new ticket
1. Classify: billing, access, integration, or other
2. Check MEMORY.md for a matching prior resolution
3. Attempt resolution; document steps taken
4. If resolved: close ticket and log pattern to memory
5. If unresolved after 3 turns: escalate per RULES.md
```

### Key points to emphasize

- `agent.yaml` is the wiring; identity files are the character
- SOUL.md is read on every query — keep it focused and specific
- RULES.md constraints are enforced via the agent's reasoning, not code — keep rules unambiguous
- These files are committed to git, so you get a full audit trail of every personality change

---

## Section 5 — Tools & Skills (15 min)

### What to say

"Tools are the actions your agent can take. Skills are composable instruction modules — think of them as prompts-plus-scripts you can snap in and invoke on demand."

### What to show

#### Built-in tools

| Tool | What it does |
|---|---|
| `cli` | Run any shell command |
| `read` | Read files from the filesystem |
| `write` | Write or create files |
| `memory` | Save to `memory/MEMORY.md` (auto-commits) |
| `capture_photo` | Take a photo via webcam |
| `task_tracker` | Create and update tasks |
| `skill_learner` | Learn and save new skills automatically |

Enable them in `agent.yaml`:

```yaml
tools:
  - cli
  - read
  - write
  - memory
  - task_tracker
```

Or restrict them from the SDK:

```typescript
import { query } from "gitagent";

for await (const msg of query({
  prompt: "Summarize the logs",
  dir: "./my-agent",
  allowedTools: ["read", "memory"],       // whitelist
  disallowedTools: ["cli"],               // or blacklist
})) {
  if (msg.type === "delta") process.stdout.write(msg.content);
}
```

#### Creating your first custom skill

"Skills live in `skills/<name>/SKILL.md`. The frontmatter registers the skill; the markdown body becomes the agent's instructions when the skill is invoked."

```bash
mkdir -p ~/my-first-agent/skills/summarize-pr
```

Create `skills/summarize-pr/SKILL.md`:

```markdown
---
name: summarize-pr
description: Summarizes a GitHub pull request — what changed, why, and risk level.
---

# Summarize Pull Request

When this skill is invoked:

1. Ask for the PR number if not provided.
2. Use the `cli` tool to run: `gh pr view <number> --json title,body,files,additions,deletions`
3. Analyze the diff for:
   - What problem it solves
   - What files changed and why
   - Estimated risk level: Low / Medium / High
   - Any obvious issues or missing test coverage
4. Output a summary in this format:

**PR #<number>: <title>**
- **What:** <one sentence>
- **Why:** <one sentence>
- **Changed:** <N files, +X -Y lines>
- **Risk:** Low / Medium / High
- **Flags:** <any issues, or "None">
```

#### Invoke skills from the REPL

```bash
# In the gitagent REPL or web UI chat:
/skill:summarize-pr Review PR #142
```

#### Skills with supporting scripts

```bash
mkdir -p ~/my-first-agent/skills/run-tests/scripts
```

Create `skills/run-tests/SKILL.md`:

```markdown
---
name: run-tests
description: Runs the project test suite and summarizes failures.
---

# Run Tests

Execute the test script and report results:

```bash
bash scripts/run.sh
```

Summarize: how many passed, how many failed, and what the failures are.
```

Create `skills/run-tests/scripts/run.sh`:

```bash
#!/usr/bin/env bash
npm test 2>&1 | tail -30
```

Scripts receive args as JSON on stdin and return output on stdout.

#### Automatic skill learning

"The `skill_learner` built-in tool is interesting. When you enable it, the agent can learn new skills from conversation and save them automatically with a confidence score. If you show it how to do something once, it can codify that as a reusable skill."

```yaml
# agent.yaml
tools:
  - cli
  - read
  - write
  - memory
  - skill_learner   # enables automatic skill capture
```

"The agent won't just save anything — it assigns confidence scores and only promotes high-confidence patterns to permanent skills. Lower confidence entries stay as memory notes until they're validated through repeated use."

### Key points to emphasize

- Skills are version-controlled — you can review, rollback, or branch skill changes
- A skill is just markdown + optional scripts, so anyone on the team can write or edit one
- `/skill:name` invocation works in both the REPL and the web UI chat

---

## Section 6 — Memory System (10 min)

### What to say

"Memory in GitAgent is unlike any other framework I've seen. Most agents use a vector database or hidden in-memory state. GitAgent's memory is a markdown file in your repo that the agent commits every time it saves something. Your agent's memory has a git history.

That means you can `git log memory/MEMORY.md` and see every memory entry in order. You can `git diff HEAD~5 memory/MEMORY.md` to see exactly what the agent remembered over the last five runs. You can `git revert` to roll back a bad memory. You can fork a repo and give the fork a completely different memory history. This is extraordinarily powerful for debugging, auditing, and collaboration."

### What to show

#### Primary memory — MEMORY.md

```bash
cat ~/my-first-agent/memory/MEMORY.md
```

"Every time the agent calls the `memory` tool, it appends to this file and creates a git commit. No external database required."

Example of what an agent writes to memory:

```markdown
# Agent Memory

## Resolved Patterns

### Billing dispute: duplicate charge
- Root cause: race condition in payment processor webhook
- Resolution: Void the duplicate, issue credit note, flag account for 30-day monitoring
- First seen: 2025-06-10, recurred: 2025-06-14

### API auth failure: 401 on valid token
- Root cause: Token cached before timezone-offset expiry recalculation
- Resolution: Force token refresh + advise client to add 5-min buffer to expiry
```

#### Memory layers via memory.yaml

For more advanced use, you can define layered memory:

```yaml
# memory/memory.yaml
layers:
  - name: primary
    path: memory/MEMORY.md
    description: Core working memory
  - name: journal
    path: memory/journal.md
    description: Daily activity log
  - name: mood
    path: memory/mood.md
    description: Current agent state and context
```

#### Why git-native memory is powerful

```bash
# See full memory history
git log --oneline memory/MEMORY.md

# See what the agent remembered in the last 10 runs
git diff HEAD~10 memory/MEMORY.md

# Roll back a bad memory entry
git revert <commit-hash>

# Fork the repo, fork the memory history
git checkout -b experiment
# edit SOUL.md, run agent, memory diverges independently
```

"In a team context: if two people fork the same agent and run it for a week, you can literally `git merge` their memory histories. Try doing that with a vector database."

### Key points to emphasize

- Memory is plain text + git, not a hidden opaque database
- `git log` on memory = full audit trail of agent decisions
- Layered memory lets you separate short-term working memory from long-term patterns
- Auto-archiving keeps MEMORY.md from growing unbounded — the agent summarizes old entries

---

## Section 7 — Hooks for Control & Safety (10 min)

### What to say

"Hooks are how you put guardrails on your agent without having to modify its core behavior. A hook fires at a specific lifecycle event — before a tool runs, after a failure, when a file changes — and it can block, modify, or allow the action.

This is critical for production deployments. You probably don't want an agent that can run `rm -rf` on your production server, even if it thinks it's a good idea. Hooks let you enforce that at the infrastructure level."

### What to show

#### Hook events reference

| Event | Fires when | Can block? |
|---|---|---|
| `pre_tool_use` | Before any tool executes | Yes |
| `post_tool_failure` | After a tool fails | No (logging) |
| `pre_query` | Before sending to LLM | Yes |
| `post_response` | After LLM responds | No (logging) |
| `file_changed` | A tracked file is modified | Yes |
| `on_error` | Any unhandled error | No (logging) |

#### Script-based hooks (hooks/hooks.yaml)

```bash
mkdir -p ~/my-first-agent/hooks
```

Create `hooks/hooks.yaml`:

```yaml
hooks:
  pre_tool_use:
    - script: hooks/safety-check.sh
      description: Block dangerous commands and require approval for deployments

  post_response:
    - script: hooks/audit-log.sh
      description: Log all responses to audit trail

  on_error:
    - script: hooks/alert.sh
      description: Alert team on unhandled errors
```

Create `hooks/safety-check.sh`:

```bash
#!/usr/bin/env bash

# Read context from stdin
CONTEXT=$(cat)
TOOL=$(echo "$CONTEXT" | jq -r '.tool // .toolName // ""')
COMMAND=$(echo "$CONTEXT" | jq -r '.args.command // ""')

# Block rm -rf under any circumstances
if echo "$COMMAND" | grep -qE 'rm\s+-rf|rm\s+--recursive\s+-f'; then
  echo '{"action":"block","reason":"Destructive rm -rf is not permitted. Use trash or move to a backup location instead."}'
  exit 0
fi

# Block git push --force to main/master
if echo "$COMMAND" | grep -qE 'git push.*--force.*(main|master)|git push.*-f.*(main|master)'; then
  echo '{"action":"block","reason":"Force push to main/master is not permitted. Open a PR."}'
  exit 0
fi

# Require human approval for deploy commands
if echo "$COMMAND" | grep -qE 'kubectl apply|helm upgrade|terraform apply|fly deploy'; then
  echo '{"action":"block","reason":"Production deployments require human approval. Use the deployment checklist PR flow."}'
  exit 0
fi

# Everything else: allow
echo '{"action":"allow"}'
```

```bash
chmod +x ~/my-first-agent/hooks/safety-check.sh
```

#### Programmatic hooks via SDK (for inline use)

```typescript
import { query } from "gitagent";

for await (const msg of query({
  prompt: "Deploy the new service version",
  dir: "./my-agent",
  hooks: {
    preToolUse: async (ctx) => {
      // Block destructive commands
      if (ctx.toolName === "cli") {
        const cmd = ctx.args.command ?? "";
        if (/rm\s+-rf/.test(cmd)) {
          return { action: "block", reason: "Destructive rm -rf blocked by policy" };
        }
        // Require approval for deploys
        if (/kubectl apply|helm upgrade/.test(cmd)) {
          return { action: "block", reason: "Deploy requires human approval via PR" };
        }
      }

      // Rewrite unsafe file writes to a sandboxed path
      if (ctx.toolName === "write" && !ctx.args.path.startsWith("/workspace/")) {
        return {
          action: "modify",
          args: { ...ctx.args, path: `/workspace/${ctx.args.path}` },
        };
      }

      return { action: "allow" };
    },

    onError: async (ctx) => {
      // Send alert — could call a webhook here
      console.error(`[ALERT] Agent error: ${ctx.error}`);
    },
  },
})) {
  if (msg.type === "delta") process.stdout.write(msg.content);
}
```

"The three hook return values are:
- `{ action: 'allow' }` — proceed normally
- `{ action: 'block', reason: '...' }` — stop the tool call, show reason to agent
- `{ action: 'modify', args: {...} }` — let the tool run but with different arguments"

### Key points to emphasize

- Hooks are the safety layer between the agent and the world
- `pre_tool_use` is the most important hook — it runs before any tool executes
- Scripts are simpler for ops teams; programmatic hooks are better for complex conditional logic
- Hooks compose — you can have multiple scripts registered for the same event

---

## Section 8 — MCP Client Integration (10 min)

### What to say

"MCP stands for Model Context Protocol — it's an open standard for connecting AI models to external tools and data sources. Think of it like a plugin system that any MCP-compatible agent can use.

GitAgent is an MCP client. Point it at any MCP server and that server's tools are automatically discovered and available to your agent, no integration code required. There's already a large ecosystem of ready-made MCP servers for GitHub, Slack, PostgreSQL, filesystem operations, web fetch, and more."

### What to show

#### Configure MCP servers in agent.yaml

```yaml
# agent.yaml
mcp_servers:
  # Local server launched as a child process (stdio transport)
  github:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_PERSONAL_ACCESS_TOKEN: "${GITHUB_PAT}"
    timeoutMs: 30000

  # Remote server over Streamable HTTP
  analytics:
    type: http
    url: "https://mcp.yourcompany.com/mcp"
    headers:
      Authorization: "Bearer ${ANALYTICS_TOKEN}"

  # Legacy SSE transport (deprecated but still supported)
  legacy-service:
    type: sse
    url: "https://old.example.com/sse"
```

#### How tool namespacing works

"When GitAgent connects to the `github` MCP server, it discovers all the tools that server exposes and registers them as `github__<tool_name>`. So `read_file` becomes `github__read_file`, `create_pr` becomes `github__create_pr`. This prevents naming collisions when you have multiple MCP servers connected."

```
MCP server: github
  └─ list_pulls         → agent sees: github__list_pulls
  └─ create_issue       → agent sees: github__create_issue
  └─ get_pull_request   → agent sees: github__get_pull_request

MCP server: analytics
  └─ query              → agent sees: analytics__query
  └─ get_dashboard      → agent sees: analytics__get_dashboard
```

#### Practical example: GitHub MCP server

```bash
# Install the GitHub MCP server
npm install -g @modelcontextprotocol/server-github

# Add to agent.yaml
```

```yaml
mcp_servers:
  github:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_PERSONAL_ACCESS_TOKEN: "${GITHUB_PAT}"
```

```bash
export GITHUB_PAT="ghp_yourtoken"

# Now ask the agent something that requires GitHub
gitagent "List the open PRs on our main repo and summarize what each one is doing"
```

"The agent will call `github__list_pulls`, `github__get_pull_request`, etc., automatically — no code, no glue layer."

#### MCP via the SDK

```typescript
import { query } from "gitagent";

for await (const msg of query({
  prompt: "Summarize last week's signups from the database",
  mcpServers: {
    postgres: {
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-postgres", process.env.DB_URL!],
    },
  },
})) {
  if (msg.type === "tool_use") console.log(`Calling: ${msg.toolName}`);
  if (msg.type === "delta") process.stdout.write(msg.content);
}
```

"SDK-level `mcpServers` merge with `agent.yaml` `mcp_servers`. If there's a key collision, the SDK value wins. This lets you override per-query without touching the manifest."

### Behavior guarantees to know about

| Behavior | Detail |
|---|---|
| Fail-soft | A server that can't start is logged and skipped — other tools keep working |
| Namespaced | Tool names are prefixed with server name, cleaned to satisfy provider naming rules |
| Pagination | Servers that paginate tool lists are fully enumerated |
| Cleanup | Stdio child processes are shut down on every exit path |
| Lazy loading | If no MCP servers are configured, the MCP SDK is never loaded |

### Key points to emphasize

- MCP is the "npm for agent tools" — install a server, point your agent at it, done
- Namespacing (`server__tool`) prevents conflicts when using multiple servers
- Fail-soft means a broken MCP server won't take down your agent session

---

## Section 9 — Going to Production (10 min)

### What to say

"At some point you're going to want to deploy an agent that runs continuously, handles real workloads, and operates in an audited environment. Let's cover the production-readiness checklist."

### What to show

#### 1. Password-protect the web UI

```bash
# Set before launching
export GITAGENT_PASSWORD="your-secure-password"
gitagent --voice   # Now localhost:3333 requires this password
```

"Anyone who can reach the web UI can chat with your agent. In production, either set a strong password, put it behind a VPN/reverse proxy, or don't expose the UI at all and use the CLI/SDK only."

#### 2. Branch-based deployment strategy

"Because your agent is a git repo, you can use branches exactly like you do for code."

```bash
# main branch = production agent
git checkout main
gitagent --dir . "Help the customer"

# feature branch = experiment safely
git checkout -b experiment/new-personality
# edit SOUL.md, RULES.md
gitagent --dir . "Test the new behavior"

# Merge when ready — peer review the SOUL.md diff just like code review
git checkout main
git merge experiment/new-personality
```

"This means your agent changes go through code review. Someone changes RULES.md to remove a safety constraint? That's a diff in a PR. Your team reviews it. CI can run tests against it. You get the same safety net you have for application code."

#### 3. Compliance and audit logging

```yaml
# agent.yaml
compliance:
  risk_level: high              # low | medium | high
  human_in_the_loop: true       # pause and require human approval for high-risk actions
  data_classification: confidential
  regulatory_frameworks: [SOC2, GDPR, HIPAA]
  recordkeeping:
    audit_logging: true
    retention_days: 90
```

"Audit logs are written to `.gitagent/audit.jsonl` — JSONL format, one entry per tool invocation, with full traces. If you need to answer 'what did the agent do at 14:32 on June 15?' you have a complete record."

#### 4. Schedules for recurring tasks

Create a schedule file:

```bash
mkdir -p ~/my-first-agent/schedules
```

Create `schedules/daily-triage.yaml`:

```yaml
name: daily-triage
description: Morning ticket triage at 8 AM every weekday
cron: "0 8 * * 1-5"          # 8 AM Mon–Fri
prompt: |
  Review all open support tickets from the last 24 hours.
  For each: classify severity, check MEMORY.md for prior similar issues,
  and prepare a triage summary. Save the summary to memory.
enabled: true
```

"Manage schedules in the web UI under the **Scheduler** tab, or define them as YAML files in the `schedules/` directory. Cron syntax, one-time runs, and recurring are all supported."

#### 5. E2B sandbox for untrusted code execution

```bash
# Run agent in an isolated VM sandbox
gitagent --sandbox "Analyze this uploaded CSV and generate a report"
```

```yaml
# Or in agent.yaml for a specific environment config
runtime:
  max_turns: 40
  sandbox: true
```

#### 6. Secrets management

```bash
# .gitignore — this is non-negotiable
cat >> ~/.gitignore_global << 'EOF'
.env
.env.*
*.pem
*.key
secrets/
EOF

git config --global core.excludesfile ~/.gitignore_global
```

"And use the global env fallback for keys that apply to all your agents:"

```bash
mkdir -p ~/.gitagent
echo 'ANTHROPIC_API_KEY=sk-ant-...' >> ~/.gitagent/.env
echo 'LYZR_API_KEY=lyzr-sk-...' >> ~/.gitagent/.env
```

"Keys in `~/.gitagent/.env` are available to all your agents without being in any individual repo. The web UI also lets you save keys via the Settings tab, and they auto-reload without restarting the server."

### Key points to emphasize

- Branch-based deployment means agent changes get the same review process as code
- Audit logs in `.gitagent/audit.jsonl` are your compliance paper trail
- `~/.gitagent/.env` keeps secrets out of individual repos
- Schedules let you turn an interactive agent into an autonomous worker

---

## Section 10 — Hands-on Exercise (15 min)

### What to say

"Now it's your turn. Each person is going to create their own agent, give it a personality and rules, and write one custom skill. By the end of this exercise you'll have a working agent you can take back to your team."

### Exercise steps

#### Step 1: Create your agent directory (2 min)

```bash
mkdir ~/firstsource-<yourname>-agent
cd ~/firstsource-<yourname>-agent
git init
```

#### Step 2: Create agent.yaml (2 min)

```yaml
# agent.yaml
spec_version: "0.1.0"
name: <yourname>-agent
version: 0.1.0
description: My First Source GitAgent

model:
  preferred: "anthropic:claude-sonnet-4-6"   # or your preferred provider
  fallback:
    - "openai:gpt-4.1-mini"

tools:
  - cli
  - read
  - write
  - memory

runtime:
  max_turns: 20
```

#### Step 3: Write your SOUL.md (3 min)

"Write a SOUL.md for yourself as if you were describing your working style to a new team member. Be specific — what do you care about, how do you communicate, what's your expertise."

```markdown
# <Your Agent Name>

You are <name>, a <role> at First Source.

## How you work
- <3 specific behavioral traits>

## Tone
- <How you communicate>

## Domain expertise
- <What you know>
```

#### Step 4: Write your RULES.md (2 min)

```markdown
# Rules

1. **Read before modifying.** Always read a file before editing it.
2. **No credentials in memory.** Never store API keys or passwords.
3. **<Add one rule specific to your role>**
4. **Report failures honestly.** If something didn't work, say so.
```

#### Step 5: Create a custom skill (4 min)

"Create a skill that's useful for your actual work. Here are some ideas:
- `standup-summary` — summarizes what you did today from git log + notes
- `code-review-checklist` — runs through a standard review checklist
- `ticket-template` — generates a properly formatted support ticket
- `api-health-check` — pings a list of endpoints and reports status"

```bash
mkdir -p skills/my-skill
```

```markdown
---
name: my-skill
description: <one sentence describing what this skill does>
---

# <Skill Name>

When this skill is invoked:

1. <Step one>
2. <Step two>
3. Output the result in this format: <format>
```

#### Step 6: Run your agent and invoke the skill (2 min)

```bash
gitagent "Hello — tell me who you are"

# Then invoke your skill:
# /skill:my-skill <input>
```

#### Share with the group

"Once everyone has their skill working, take two minutes to share: what skill did you build, and what would it actually save you time on?"

---

## Section 11 — Q&A (10 min)

### Anticipated questions with answers

**Q: Can multiple developers share one agent repo?**

Yes — that's the point. Treat it like a shared service repo. Use branch protection on `main`, require PR reviews for changes to `SOUL.md`, `RULES.md`, or `hooks/`. Anyone can add skills on feature branches.

**Q: How do I handle secrets in a shared agent repo?**

Two approaches:
1. Use environment variable references in agent.yaml (`"${MY_KEY}"`) and have each developer set the var locally or in CI
2. Put team-level secrets in `~/.gitagent/.env` on each machine — never committed

Never put actual key values in any tracked file.

**Q: What happens when the agent runs out of turns?**

It stops with a `max_turns` system message. The state (including memory) is preserved. You can resume the conversation by running the agent again — it'll read MEMORY.md and have context.

**Q: Can I use GitAgent with our existing CI/CD pipeline?**

Yes. The SDK is the right approach for CI integration:

```typescript
// In your CI script
import { query } from "gitagent";

for await (const msg of query({
  prompt: `Review PR #${process.env.PR_NUMBER} for security issues`,
  dir: "./agent",
  model: "anthropic:claude-sonnet-4-6",
  allowedTools: ["read", "cli"],   // restrict for CI
})) {
  if (msg.type === "assistant") console.log(msg.content);
}
```

**Q: How does SkillFlow work for multi-step workflows?**

SkillFlows are YAML files that define multi-step workflows. They support `__approval_gate__` steps that pause execution and ping via Telegram or WhatsApp before continuing. Good for workflows where a human needs to review an intermediate result before the agent proceeds. Manage them in the web UI's **SkillFlows** tab.

**Q: Can the agent talk to our internal tools, not just public MCP servers?**

Yes. Write a simple MCP server that wraps your internal API (there are SDKs for Python, TypeScript, and more at [modelcontextprotocol.io](https://modelcontextprotocol.io)) and configure it as a `stdio` server in `agent.yaml`. It runs as a child process on the same machine — no public exposure needed.

**Q: What's the difference between `DUTIES.md` and a scheduled task?**

DUTIES.md tells the agent what it's responsible for conceptually — it shapes behavior during any session. A schedule actually triggers the agent to run at a specific time. You'd typically have related content in both: DUTIES.md says "you own daily triage", and a schedule actually runs the triage at 8 AM.

**Q: Can I connect GitAgent to our Lyzr Studio agents programmatically in a script?**

Yes:

```typescript
import { query } from "gitagent";

for await (const msg of query({
  prompt: "Handle this support request",
  model: `lyzr:${process.env.LYZR_AGENT_ID}@https://agent-prod.studio.lyzr.ai/v4`,
  dir: "./my-agent",
})) {
  if (msg.type === "delta") process.stdout.write(msg.content);
}
```

Set `LYZR_API_KEY` in the environment before running.

---

## Quick Reference Card

Save this for daily use:

```bash
# Install
bash <(curl -fsSL "https://raw.githubusercontent.com/open-gitagent/gitagent/main/install.sh?$(date +%s)")
npm install -g @open-gitagent/gitagent @open-gitagent/voice

# Run
gitagent "prompt"                                    # run in current dir
gitagent --dir ~/my-agent "prompt"                   # specific dir
gitagent --model anthropic:claude-sonnet-4-6 "prompt"   # override model
gitagent --voice                                     # open web UI at localhost:3333
gitagent --sandbox "prompt"                          # run in isolated VM

# Lyzr Studio
export LYZR_API_KEY="lyzr-sk-..."
gitagent --model "lyzr:<agent-id>@https://agent-prod.studio.lyzr.ai/v4" "prompt"

# Invoke a skill
/skill:my-skill <input>

# Plugins
gitagent plugin install https://github.com/org/plugin.git
gitagent plugin list
gitagent plugin init my-plugin

# Telemetry (optional)
OTEL_TRACES_EXPORTER=console gitagent "prompt"       # print spans to stdout
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 gitagent "prompt"  # Jaeger

# Git memory inspection
git log --oneline memory/MEMORY.md                   # see memory history
git diff HEAD~5 memory/MEMORY.md                     # see recent memory changes
```

### Key environment variables

| Variable | Purpose |
|---|---|
| `ANTHROPIC_API_KEY` | Anthropic / Claude |
| `OPENAI_API_KEY` | OpenAI |
| `GOOGLE_GENERATIVE_AI_API_KEY` | Google Gemini |
| `GROQ_API_KEY` | Groq |
| `LYZR_API_KEY` | Lyzr Studio |
| `GITHUB_TOKEN` | GitHub repo access |
| `GITAGENT_PASSWORD` | Web UI password |
| `GITAGENT_SLIM` | Set to `1` to skip voice on install |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OpenTelemetry collector URL |

### Agent directory structure reference

```
my-agent/
├── agent.yaml              # Required: model, tools, runtime, compliance
├── SOUL.md                 # Personality and identity
├── RULES.md                # Behavioral constraints
├── DUTIES.md               # Recurring responsibilities
├── AGENTS.md               # Sub-agent relationships
├── memory/
│   ├── MEMORY.md           # Primary memory (auto-committed)
│   ├── memory.yaml         # Memory layer config (optional)
│   ├── mood.md             # Agent state (optional)
│   └── journal.md          # Activity log (optional)
├── skills/
│   └── <name>/
│       ├── SKILL.md        # Skill definition (frontmatter + instructions)
│       └── scripts/        # Supporting scripts
├── hooks/
│   └── hooks.yaml          # Lifecycle hook scripts
├── tools/
│   └── *.yaml              # Declarative tool definitions
├── plugins/
│   └── <name>/             # Local plugins
├── schedules/
│   └── *.yaml              # Cron schedule definitions
└── .gitagent/
    └── audit.jsonl         # Audit log (when audit_logging: true)
```

---

## Resources

- GitHub: [https://github.com/open-gitagent/gitagent](https://github.com/open-gitagent/gitagent)
- Lyzr Studio: [https://studio.lyzr.ai](https://studio.lyzr.ai)
- MCP servers: [https://modelcontextprotocol.io](https://modelcontextprotocol.io)
- Issues / support: [https://github.com/open-gitagent/gitagent/issues](https://github.com/open-gitagent/gitagent/issues)

---

*Session prepared for First Source dev team onboarding — GitAgent v1.1.1+*
