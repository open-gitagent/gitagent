# gitagent Command Reference

## Commands

| Command | Description |
|---------|-------------|
| `gitagent init` | Scaffold a new agent repo (`-t minimal\|standard\|full`) |
| `gitagent validate` | Validate agent.yaml and structure (`-c` for compliance) |
| `gitagent info` | Show agent summary |
| `gitagent export` | Export to other formats (`-f system-prompt\|claude-code\|openai\|crewai\|openclaw\|nanobot\|lyzr\|github`) |
| `gitagent import` | Import from other frameworks (`--from claude\|cursor\|crewai`) |
| `gitagent install` | Resolve git dependencies |
| `gitagent audit` | Generate compliance audit report |
| `gitagent skills search` | Search skill registries |
| `gitagent skills install` | Install a skill |
| `gitagent skills list` | List discovered skills |
| `gitagent skills info` | Inspect a skill |
| `gitagent run` | Run agent with an adapter (`-a claude\|openai\|crewai\|openclaw\|nanobot\|lyzr\|github\|git\|prompt`) |
| `gitagent lyzr create` | Create agent on Lyzr Studio |
| `gitagent lyzr update` | Push changes to Lyzr |
| `gitagent lyzr info` | Show linked Lyzr agent ID |
| `gitagent lyzr run` | Clone + create + chat on Lyzr in one command |

## Common Flags

| Flag | Commands | Description |
|------|----------|-------------|
| `-d, --dir <dir>` | All | Agent directory (default: `.`) |
| `-r, --repo <url>` | run, lyzr run | Git repository URL |
| `-a, --adapter <name>` | run | Adapter selection |
| `-p, --prompt <msg>` | run, lyzr run | Initial prompt |
| `-b, --branch <branch>` | run, lyzr run | Git branch (default: `main`) |
| `--refresh` | run, lyzr run | Force re-clone |
| `--no-cache` | run | Clone to temp dir |
| `-f, --format <fmt>` | export | Export format |
| `-o, --output <path>` | export | Output file |
| `-t, --template <name>` | init | Template type |
| `-c, --compliance` | validate | Enable compliance checks |
| `--api-key <key>` | lyzr * | Lyzr API key override |

## Environment Variables

| Variable | Adapters |
|----------|----------|
| `ANTHROPIC_API_KEY` | claude, openclaw, nanobot |
| `OPENAI_API_KEY` | openai |
| `LYZR_API_KEY` | lyzr |
| `GITHUB_TOKEN` / `GH_TOKEN` | github |

## Required Files

- `agent.yaml` — Always required
- `SOUL.md` — Always required
- Everything else is optional

## Directory Cheat Sheet

```
agent.yaml       → Identity, model, skills, tools, compliance
SOUL.md          → Personality, values, expertise
RULES.md         → Hard constraints, boundaries
PROMPT.md        → Default task framing
skills/          → Reusable capability modules
tools/           → MCP-compatible tool schemas
knowledge/       → Reference documents
memory/          → Cross-session memory
hooks/           → Lifecycle event handlers
agents/          → Sub-agent definitions
compliance/      → Regulatory artifacts
config/          → Environment overrides
workflows/       → Multi-step procedures
```
