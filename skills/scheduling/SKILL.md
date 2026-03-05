---
name: scheduling
description: Create and manage scheduled jobs that run as GitHub Actions — schedule agents, commands, code reviews, linting, and any recurring automation
license: MIT
allowed-tools: gitcron cli read write
metadata:
  author: open-gitagent
  version: "1.0.0"
  category: automation
---

# Scheduling

## Instructions

You can create and manage scheduled jobs that run automatically as GitHub Actions workflows. Each schedule is defined in `cron.yaml` and compiled to `.github/workflows/` files.

### When to create a schedule

Create a schedule when the user wants something to run:
- On a recurring basis (nightly, weekly, monthly)
- Automatically without human intervention
- As a GitHub Actions workflow

### Schedule types

**Agent schedules** — Run a gitagent/gitclaw AI agent:
```yaml
- name: nightly-review
  cron: "0 2 * * *"
  agent: code-reviewer
  adapter: claude        # or: openai, gitclaw, system-prompt
  prompt: "Review all open PRs"
  branch:
    strategy: pr         # creates a PR with the agent's changes
```

**Command schedules** — Run any shell command:
```yaml
- name: weekly-lint
  cron: "0 6 * * 1"
  command: "npm run lint -- --fix"
  branch:
    strategy: commit     # commits directly to base branch
```

### Cron expression reference

```
┌───────────── minute (0-59)
│ ┌─────────── hour (0-23)
│ │ ┌───────── day of month (1-31)
│ │ │ ┌─────── month (1-12)
│ │ │ │ ┌───── day of week (0-7, Sun=0 or 7)
│ │ │ │ │
* * * * *
```

Common patterns:
- `0 2 * * *` — Daily at 2 AM
- `0 9 * * 1` — Every Monday at 9 AM
- `0 0 1 * *` — First of every month at midnight
- `*/15 * * * *` — Every 15 minutes
- `0 9 1 */3 *` — Quarterly (first of every 3rd month)

### Branch strategies

| Strategy | Use when |
|----------|----------|
| `pr` | Agent makes code changes that need review |
| `create` | Push a branch without opening a PR |
| `commit` | Small, safe changes (lint fixes, formatting) |
| `none` | Read-only tasks (audits, reports, scans) |

### Workflow

1. Use `gitcron` tool with `schedule-list` to see existing schedules
2. Use `gitcron` tool with appropriate command to create/modify schedules
3. Edit `cron.yaml` directly for complex configurations
4. Run `gitcron generate` to compile to GitHub Actions workflows
5. The workflows will run automatically after push to the repository

### Adapter selection

- **claude** — Best for code changes, complex reasoning. Uses Claude Code via gitagent.
- **gitclaw** — Full agent runtime with tools, hooks, audit logging, compliance. Best for enterprise use.
- **openai** — OpenAI API integration via gitagent.
- **system-prompt** — Generic LLM export.

## Output Format

When creating a schedule, confirm:
1. Schedule name and cron expression (human-readable)
2. What it does (agent/command)
3. Branch strategy chosen and why
4. Next steps: `gitcron generate` and push
