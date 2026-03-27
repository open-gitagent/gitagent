# Running GitClaw inside NVIDIA OpenShell

## The Problem: AI Agents Are Powerful — And That's Scary

AI agents today can read your files, write code, run shell commands, send messages, and call cloud APIs — all autonomously, with minimal human oversight. That's incredibly useful. It's also incredibly risky if you don't have guardrails.

In March 2026, NVIDIA released [OpenShell](https://github.com/NVIDIA/OpenShell) — an open-source sandboxed runtime designed specifically for AI agents. Think of it as a secure container that wraps around an agent and controls exactly what it can and can't do: which files it reads, which APIs it calls, what privileges it has, and whether it gets GPU access. Everything is defined in a simple YAML configuration file, and every blocked action is logged.

OpenShell already ships with support for several agents, including OpenClaw (the viral general-purpose agent with 250K+ GitHub stars). But here's the thing — **OpenClaw has serious security problems**, and GitClaw was purpose-built to avoid them.

## Why GitClaw over OpenClaw?

OpenClaw is impressive in breadth. It connects to 20+ messaging channels, has 13,700+ community skills, and can orchestrate almost anything. But that breadth comes with significant trade-offs:

**Security is OpenClaw's Achilles' heel.** Authentication is disabled by default. Credentials are stored in plaintext config files. The ClawHub skills marketplace has been found to contain malicious payloads in up to 20% of listed skills — credential theft, data exfiltration, backdoors. Microsoft, Cisco, Kaspersky, and multiple universities have published security advisories warning against running it on standard workstations. A high-severity CVE (CVE-2026-25253, CVSS 8.8) showed the Control UI auto-transmitting auth tokens to attacker-controlled WebSocket URLs. Prompt injection is described as an architectural vulnerability that "cannot be fully solved" in OpenClaw's design.

**GitClaw takes a different approach.** It's built as a focused, git-native agent — not a general-purpose life assistant. Here's how they compare:

| | GitClaw | OpenClaw |
|---|---|---|
| **Primary purpose** | Autonomous coding & project agent | General-purpose life/work assistant |
| **Security model** | Git-native (all changes tracked, reversible), sandboxed CLI tool execution, auditable | Auth disabled by default, plaintext credentials, vulnerable skill marketplace |
| **Voice mode** | Real-time bidirectional voice with OpenAI Realtime API, camera/screen input, photo capture | TTS/STT via ElevenLabs, voice notes, no real-time bidirectional |
| **Skills** | Curated skills marketplace, skill learning (agent creates its own skills), SkillsFlow visual workflow builder | 13,700+ community skills (but ~20% flagged as malicious) |
| **Memory** | Structured git-committed memory with reinforcement learning, memory archival | Markdown diary entries |
| **Multi-channel** | Voice UI, Telegram, WhatsApp | 20+ channels |
| **Agent brain** | Pluggable (Claude, GPT, Gemini, Ollama, etc.) | Pluggable (similar range) |
| **Architecture** | Single focused process, SDK for embedding | Gateway + multiple services |

GitClaw is narrower in scope but deeper in execution. It won't manage your Slack DMs or order you coffee, but it will autonomously write, test, and ship code — with every change committed to git, every tool call hookable, and every action auditable.

## Why GitClaw + OpenShell?

Even though GitClaw is already more security-conscious than OpenClaw, adding OpenShell on top gives you defense-in-depth:

- **Network isolation.** GitClaw only reaches the APIs you explicitly allow — Anthropic for reasoning, OpenAI for voice, nothing else. Default-deny networking at the kernel level.
- **Filesystem boundaries.** The agent can read and write your project folder. It cannot touch anything else on the machine. Enforced via Linux Landlock LSM and seccomp, not just application-level checks.
- **Non-root execution.** The agent runs as a sandboxed user, never as administrator. Even if something goes wrong, it can't escalate privileges.
- **Hot-reloadable policies.** Tighten or loosen the rules while the agent is running. Start permissive (audit mode), then lock down once you're confident.
- **Full audit trail.** Every blocked action is logged with the exact binary, target, and reason. Compliance teams and security reviewers can see precisely what happened.

This matters for enterprise teams deploying GitClaw across developers, regulated industries (banking, healthcare, government) that need clear access boundaries, multi-tenant setups where each user gets an isolated instance, and CI/CD pipelines where agents run unattended.

OpenShell turns GitClaw from "an AI that can do a lot on your machine" into "an AI that can do exactly what you've approved, and nothing else."

## What We'll Set Up

This guide walks through everything step by step:

1. **Install OpenShell** on your machine (it runs on top of Docker)
2. **Build a sandbox** — a secure container with GitClaw pre-installed
3. **Write a security policy** — the rules controlling what GitClaw can and can't do
4. **Launch GitClaw in the sandbox** — with voice mode, port forwarding, and API access
5. **Monitor and adjust** — view logs, check blocked actions, and tweak the policy

No prior experience with Docker or security tooling is required — every command is included below.

## Prerequisites

- Docker running on the host
- OpenShell CLI installed:
  ```bash
  curl -LsSf https://raw.githubusercontent.com/NVIDIA/OpenShell/main/install.sh | sh
  ```
- API keys: `OPENAI_API_KEY` (voice), `ANTHROPIC_API_KEY` (agent)

## Quick Start

```bash
# Create sandbox from a local directory with port forwarding for voice
openshell sandbox create \
  --from ./sandboxes/gitclaw \
  --policy ./sandboxes/gitclaw/policy.yaml \
  --forward 3333 \
  --name gitclaw-dev \
  -- gitclaw --voice --dir /sandbox/project

# Open the voice UI
open http://localhost:3333
```

## Sandbox Structure

Create the following directory:

```
sandboxes/gitclaw/
  Dockerfile
  policy.yaml
```

### Dockerfile

```dockerfile
ARG BASE_IMAGE=ghcr.io/nvidia/openshell-community/sandboxes/base:latest
FROM ${BASE_IMAGE}

USER root

# Install gitclaw globally
RUN npm install -g gitclaw@latest

# Create workspace
RUN mkdir -p /sandbox/project && chown -R sandbox:sandbox /sandbox

USER sandbox
WORKDIR /sandbox/project
ENTRYPOINT ["/bin/bash"]
```

### policy.yaml

```yaml
version: 1

filesystem_policy:
  include_workdir: true
  read_only:
    - /usr
    - /lib
    - /proc
    - /dev/urandom
    - /etc
  read_write:
    - /sandbox
    - /tmp
    - /dev/null

landlock:
  compatibility: best_effort

process:
  run_as_user: sandbox
  run_as_group: sandbox

network_policies:
  anthropic_api:
    name: anthropic-api
    endpoints:
      - host: api.anthropic.com
        port: 443
        protocol: rest
        tls: terminate
        enforcement: enforce
        access: full
    binaries:
      - path: /usr/local/bin/node

  openai_api:
    name: openai-api
    endpoints:
      - host: api.openai.com
        port: 443
        protocol: rest
        tls: terminate
        enforcement: enforce
        access: full
    binaries:
      - path: /usr/local/bin/node

  openai_realtime:
    name: openai-realtime
    endpoints:
      - host: api.openai.com
        port: 443
        protocol: wss
        tls: terminate
        enforcement: enforce
        access: full
    binaries:
      - path: /usr/local/bin/node

  npm_registry:
    name: npm-registry
    endpoints:
      - host: registry.npmjs.org
        port: 443
    binaries:
      - path: /usr/local/bin/npm
```

**Key points:**
- Default-deny networking — only the endpoints listed above are reachable
- Filesystem uses Landlock LSM + seccomp — anything not listed is inaccessible
- Process runs as `sandbox` user, never root
- Voice mode needs the `openai_realtime` WebSocket endpoint

## Uploading Your Project

```bash
# Upload an existing agent directory into the sandbox
openshell sandbox upload gitclaw-dev ./my-agent /sandbox/project

# Or create a fresh agent inside the sandbox
openshell sandbox connect gitclaw-dev
# Then inside: gitclaw --voice --dir /sandbox/project
```

## Port Forwarding (Voice Mode)

GitClaw's voice server runs on port 3333. Forward it to your host:

```bash
# At creation time (shown in Quick Start above)
openshell sandbox create --forward 3333 ...

# Or add forwarding to a running sandbox
openshell forward start 3333 gitclaw-dev

# Background mode
openshell forward start 3333 gitclaw-dev -d

# List active forwards
openshell forward list

# Stop
openshell forward stop 3333 gitclaw-dev
```

Then open `http://localhost:3333` in your browser.

## Environment Variables

Pass API keys when creating the sandbox:

```bash
openshell sandbox create \
  --from ./sandboxes/gitclaw \
  --env OPENAI_API_KEY="sk-..." \
  --env ANTHROPIC_API_KEY="sk-ant-..." \
  --forward 3333 \
  --name gitclaw-dev
```

Or place a `.env` file in the project directory before uploading — GitClaw's `install.sh` and `server.ts` will pick it up automatically.

## GPU Passthrough

If running local inference (e.g., Ollama models instead of API calls):

```bash
openshell sandbox create --gpu --from ./sandboxes/gitclaw --name gitclaw-gpu
```

Add Ollama to the policy if needed:

```yaml
  ollama_local:
    name: ollama
    endpoints:
      - host: host.docker.internal
        port: 11434
        protocol: rest
        enforcement: enforce
        access: full
    binaries:
      - path: /usr/local/bin/node
```

## Monitoring & Debugging

```bash
# Stream sandbox logs
openshell logs gitclaw-dev --tail --source sandbox

# Check for policy denials
openshell logs gitclaw-dev --level warn --since 5m

# Open the TUI dashboard (k9s-style)
openshell term
```

Denial logs show exactly which binary tried to connect where and why it was blocked — useful for iterating on the policy.

## Hot-Reload Policies

Adjust the network policy on a running sandbox without restarting:

```bash
# Export current policy
openshell policy get gitclaw-dev --full > current.yaml

# Edit current.yaml (e.g., add a new API endpoint)

# Apply
openshell policy set gitclaw-dev --policy current.yaml --wait
```

Use `enforcement: audit` during initial setup to log violations without blocking:

```yaml
    endpoints:
      - host: api.anthropic.com
        port: 443
        enforcement: audit    # log only, don't block
        access: full
```

Once everything works, switch to `enforcement: enforce`.

## Composio / Integrations

If using Composio (Gmail, Calendar, Slack, GitHub integrations), add its endpoint:

```yaml
  composio_api:
    name: composio
    endpoints:
      - host: "*.composio.dev"
        port: 443
        protocol: rest
        tls: terminate
        enforcement: enforce
        access: full
    binaries:
      - path: /usr/local/bin/node
```

Similarly for Telegram, WhatsApp, or any other integration GitClaw supports — add the relevant API hosts to `network_policies`.

## Download Results

Pull generated files (workspace output, memory, photos) back to your host:

```bash
openshell sandbox download gitclaw-dev /sandbox/project/workspace ./output
```
