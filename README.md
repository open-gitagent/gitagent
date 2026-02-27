# gitagent

[![npm version](https://img.shields.io/npm/v/@shreyaskapale/gitagent)](https://www.npmjs.com/package/@shreyaskapale/gitagent)
[![CI](https://github.com/open-gitagent/gitagent/actions/workflows/ci.yml/badge.svg)](https://github.com/open-gitagent/gitagent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Spec: v0.1.0](https://img.shields.io/badge/spec-v0.1.0-blue)](https://github.com/open-gitagent/gitagent/blob/main/spec/SPECIFICATION.md)
[![Node >= 18](https://img.shields.io/badge/node-%3E%3D18-brightgreen)](https://nodejs.org)

A framework-agnostic, git-native standard for defining AI agents. Clone a repo, get an agent.

## Why

Every AI framework has its own structure. There's no universal, portable way to define an agent that works across Claude Code, OpenAI, LangChain, CrewAI, and AutoGen. **gitagent** fixes that.

- **Git-native** — Version control, branching, diffing, and collaboration built in
- **Framework-agnostic** — Export to any framework with adapters
- **Compliance-ready** — First-class support for FINRA, Federal Reserve, and SEC regulatory requirements
- **Composable** — Agents can extend, depend on, and delegate to other agents

## Patterns

Four architectural patterns emerge when you treat agents as git repos:

### Human-in-the-Loop for RL Agents
When an agent updates memory or learns a new skill, it creates a branch + PR for human review before merging to main. Git's review workflow becomes your supervision layer.

### Shared Context
Root-level `context.md`, `skills/`, `tools/`, and `knowledge/` are automatically inherited by all sub-agents. One source of truth, no duplication.

### Branch-based Deployment
Use git branches (`dev` → `staging` → `main`) to promote agent changes through environments, just like shipping software.

### Knowledge Tree
The `knowledge/` folder stores entity relationships as a hierarchical tree with embeddings, letting agents reason over structured data at runtime.

## Quick Start

```bash
# Install
npm install -g gitagent

# Create a new agent
gitagent init --template standard

# Validate
gitagent validate

# View agent info
gitagent info

# Export to system prompt
gitagent export --format system-prompt
```

## Directory Structure

```
my-agent/
├── agent.yaml          # [REQUIRED] Manifest — name, version, model, skills, tools, compliance
├── SOUL.md             # [REQUIRED] Identity, personality, communication style, values
├── RULES.md            # Hard constraints, must-always/must-never, safety boundaries
├── AGENTS.md           # Framework-agnostic fallback instructions
├── skills/             # Reusable capability modules (SKILL.md + scripts)
├── tools/              # MCP-compatible tool definitions (YAML schemas)
├── knowledge/          # Reference documents the agent can consult
├── memory/             # Persistent cross-session memory
├── workflows/          # Multi-step procedures/playbooks
├── hooks/              # Lifecycle event handlers (audit logging, compliance checks)
├── examples/           # Calibration interactions (few-shot)
├── agents/             # Sub-agent definitions (recursive structure)
├── compliance/         # Regulatory compliance artifacts
├── config/             # Environment-specific overrides
└── .gitagent/          # Runtime state (gitignored)
```

## agent.yaml

The only file with a strict schema. Minimal example:

```yaml
spec_version: "0.1.0"
name: my-agent
version: 0.1.0
description: A helpful assistant agent
```

Full example with compliance:

```yaml
spec_version: "0.1.0"
name: compliance-analyst
version: 1.0.0
description: Financial compliance analysis agent
model:
  preferred: claude-opus-4-6
compliance:
  risk_tier: high
  frameworks: [finra, federal_reserve, sec]
  supervision:
    human_in_the_loop: always
    kill_switch: true
  recordkeeping:
    audit_logging: true
    retention_period: 7y
    immutable: true
  model_risk:
    validation_cadence: quarterly
    ongoing_monitoring: true
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `gitagent init [--template]` | Scaffold new agent (`minimal`, `standard`, `full`) |
| `gitagent validate [--compliance]` | Validate against spec and regulatory requirements |
| `gitagent info` | Display agent summary |
| `gitagent export --format <fmt>` | Export to other formats (see adapters below) |
| `gitagent import --from <fmt> <path>` | Import (`claude`, `cursor`, `crewai`) |
| `gitagent run <source> --adapter <a>` | Run an agent from a git repo or local directory |
| `gitagent install` | Resolve and install git-based dependencies |
| `gitagent audit` | Generate compliance audit report |
| `gitagent skills <cmd>` | Manage skills (`search`, `install`, `list`, `info`) |
| `gitagent lyzr <cmd>` | Manage Lyzr agents (`create`, `update`, `info`, `run`) |

## Compliance

gitagent has first-class support for financial regulatory compliance:

### FINRA
- **Rule 3110** — Supervision: human-in-the-loop, escalation triggers, kill switch
- **Rule 4511** — Recordkeeping: immutable audit logs, retention periods, SEC 17a-4 compliance
- **Rule 2210** — Communications: fair/balanced enforcement, no misleading statements
- **Reg Notice 24-09** — Existing rules apply to GenAI/LLMs

### Federal Reserve
- **SR 11-7** — Model Risk Management: validation cadence, ongoing monitoring, outcomes analysis
- **SR 23-4** — Third-Party Risk: vendor due diligence, SOC reports, subcontractor assessment

### SEC / CFPB
- **Reg S-P** — Customer privacy, PII handling
- **CFPB Circular 2022-03** — Explainable adverse action, Less Discriminatory Alternative search

Run `gitagent audit` for a full compliance checklist against your agent configuration.

## Adapters

Adapters are used by both `export` and `run`. Available adapters:

| Adapter | Description |
|---------|-------------|
| `system-prompt` | Concatenated system prompt (works with any LLM) |
| `claude-code` | Claude Code compatible CLAUDE.md |
| `openai` | OpenAI Agents SDK Python code |
| `crewai` | CrewAI YAML configuration |
| `lyzr` | Lyzr Studio agent |
| `github` | GitHub Actions agent |
| `git` | Git-native execution (run only) |
| `openclaw` | OpenClaw format |
| `nanobot` | Nanobot format |

```bash
# Export to system prompt
gitagent export --format system-prompt

# Run an agent directly
gitagent run ./my-agent --adapter lyzr
```

## Inheritance & Composition

```yaml
# Extend a parent agent
extends: https://github.com/org/base-agent.git

# Compose with dependencies
dependencies:
  - name: fact-checker
    source: https://github.com/org/fact-checker.git
    version: ^1.0.0
    mount: agents/fact-checker
```

## Examples

See the `examples/` directory:

- **`examples/minimal/`** — 2-file hello world (agent.yaml + SOUL.md)
- **`examples/standard/`** — Code review agent with skills, tools, and rules
- **`examples/full/`** — Production compliance agent with all directories, hooks, workflows, sub-agents, and regulatory artifacts
- **`examples/gitagent-helper/`** — Helper agent that assists with creating gitagent definitions
- **`examples/lyzr-agent/`** — Example Lyzr Studio integration

## Specification

Full specification at [`spec/SPECIFICATION.md`](spec/SPECIFICATION.md).

JSON Schemas for validation at `spec/schemas/`.

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=open-gitagent/gitagent&type=Date)](https://star-history.com/#open-gitagent/gitagent&Date)

## Built with gitagent?

If you've built an agent using gitagent, we'd love to hear about it! [Open a discussion](https://github.com/open-gitagent/gitagent/discussions) or add a `gitagent` topic to your repo.

## License

MIT
