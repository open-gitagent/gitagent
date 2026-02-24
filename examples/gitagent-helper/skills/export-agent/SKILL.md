---
name: export-agent
description: Help users export agents to other frameworks and import from existing tools
license: MIT
metadata:
  author: gitagent
  version: "1.0.0"
  category: interop
---

# Export & Import Agents

## When to Use
When a user wants to convert their agent to another framework format, or import an existing agent into gitagent.

## Export

Convert a gitagent definition to another framework:

```bash
gitagent export -f <format> -d ./my-agent [-o output-file]
```

### Formats

| Format | Output | Use Case |
|--------|--------|----------|
| `system-prompt` | Markdown | Universal — paste into any LLM |
| `claude-code` | CLAUDE.md | Drop into a Claude Code project |
| `openai` | Python | Run with OpenAI Agents SDK |
| `crewai` | YAML | Run with CrewAI |
| `openclaw` | JSON + MD | Run with OpenClaw |
| `nanobot` | JSON + MD | Run with Nanobot |
| `lyzr` | JSON | Create agent on Lyzr Studio |
| `github` | JSON | Call GitHub Models API |

### Examples

```bash
# Get a system prompt for any LLM
gitagent export -f system-prompt -d ./my-agent

# Generate a CLAUDE.md
gitagent export -f claude-code -d ./my-agent -o CLAUDE.md

# Generate Python code for OpenAI
gitagent export -f openai -d ./my-agent -o agent.py

# Preview what Lyzr API will receive
gitagent export -f lyzr -d ./my-agent

# Preview GitHub Models payload
gitagent export -f github -d ./my-agent
```

## Import

Convert existing agent frameworks into gitagent:

```bash
gitagent import --from <format> <path> [-d target-dir]
```

### Sources

| Source | Input | What It Creates |
|--------|-------|-----------------|
| `claude` | CLAUDE.md, .claude/skills/ | agent.yaml, SOUL.md, RULES.md, skills |
| `cursor` | .cursorrules | agent.yaml, SOUL.md, AGENTS.md |
| `crewai` | crew.yaml | agent.yaml, SOUL.md, agents/ |

### Examples

```bash
# Import a Claude Code project
gitagent import --from claude ./my-project

# Import from Cursor
gitagent import --from cursor ./.cursorrules

# Import CrewAI config
gitagent import --from crewai ./crew.yaml -d ./imported
```
