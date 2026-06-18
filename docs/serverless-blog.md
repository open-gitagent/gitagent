# Kill the Server. Your AI Agent Belongs in Git.

Most AI agents are servers. They sit idle, burning compute, waiting for the next request. This is not a technical requirement. It is a habit — and an expensive one.

GitAgent is not an agent you run. It is an agent you invoke.

---

## The Server Assumption

When developers build AI agents, they reach for the same mental model they use for web applications: start a process, keep it alive, let it handle requests. The model feels natural because it is familiar. But it rests on an assumption that almost nobody questions.

Agents need state. They need to remember what happened last time — what tasks are in progress, what decisions were made, what the user told them yesterday. If you kill the process, you lose the context. So the process stays alive. And a process that stays alive is a server.

To make that server durable, teams add infrastructure. Redis for short-term memory. Postgres for long-term state. S3 for file storage. A message queue so the agent survives a restart. Before long, you have a distributed system — and a monthly bill — just to keep an agent's memory alive between conversations.

The assumption nobody examined: state lives in a database, so compute must live near the database.

GitAgent challenges this at the foundation.

---

## Git Is Already a Database

Every piece of state a GitAgent agent needs is stored in a git repository. Memory is a markdown file. Task history is a structured file. Audit logs are append-only. The agent's personality, goals, and behavioral constraints live in `agent.yaml` and `SOUL.md`. Everything that must survive a session is a file that git tracks.

This is not a workaround. It is a deliberate architecture.

Git is already distributed. It is already durable — commits are fsync'd to disk before returning. It is already versioned, meaning every state change has a timestamp, an author, and a reason. It is already replicated the moment you push to a remote. It has been battle-tested as a persistence layer by millions of teams for two decades.

Every AI agent team building a custom state management layer is rebuilding something git already provides — worse, without the distribution, the versioning, or the auditability.

The agent repo is not where the code lives. It is the database, the audit log, the memory store, and the deployment artifact simultaneously.

---

## What Serverless Actually Means Here

When you invoke GitAgent in single-shot mode, a precise lifecycle runs and terminates:

The agent starts by cloning its repo and loading its identity from configuration files. It connects any declared tools and MCP servers. It runs the task. When the task completes, a `finally` block commits any state changes to git, pushes them to the remote, and exits. The process does not linger. Nothing idles. The compute existed for exactly as long as the work took.

When you invoke it again — an hour later, a week later, on a different machine — it clones the same repo, reads the same memory file, and picks up exactly where it left off. The continuity of the agent is in git, not in a running process. The compute is disposable. The state is permanent.

This is the inversion. Traditional agents keep compute alive to protect state. GitAgent makes state durable so compute can be ephemeral.

---

## Memory That Survives Process Death

The memory system is where this becomes concrete.

GitAgent writes memory to a markdown file in the agent repo. Every save is a synchronous git commit — the write is durable before the function returns. If the process crashes after the commit, the memory is not lost. It is in git history. The next invocation reads the same file and continues.

This is fundamentally different from in-memory state or a database that a long-running process manages. There is no connection to close, no transaction to roll back, no cache to warm. The state is just files. Files that git manages with the same reliability guarantees git has always provided.

When memory grows large, older entries are archived automatically — moved to a dated archive file in the same atomic commit. The agent's knowledge base is self-managing without any server process watching over it.

---

## $0 Between Runs

The cost argument follows directly from the architecture.

An always-on agent on a cloud VM costs money every hour, whether it processes one request or none. An agent on managed infrastructure — ECS, Cloud Run, Kubernetes — costs money to keep warm, to maintain availability, to replicate state. The infrastructure bill does not care whether your agent was useful today.

A GitAgent agent costs nothing between invocations. You pay only for the seconds it is actually working. On GitHub Actions, the compute is not just cheap — for most usage patterns it is free entirely. The agent runs, commits its state, and the runner shuts down. There is no idle cost because there is no idle state.

The agent repo itself is free on any public repository host. The memory, the skills, the audit log, the agent's entire history — stored at zero marginal cost in a git repository that would exist anyway.

---

## Triggering an Ephemeral Agent

Because the agent is stateless compute, any system that can run a command can trigger it. A GitHub Actions workflow on a schedule. A webhook handler in a serverless function. A CI pipeline step. A cron job on any machine. Even a developer running a one-liner from a terminal.

The trigger mechanism does not matter because the agent does not care how it was invoked. It reads its state from the repo, does the work, commits the result, and exits. The scheduler is external infrastructure — managed, reliable, already paid for — not something the agent process has to maintain.

This also means concurrent runs are naturally safe. Each session creates an isolated git branch. Ten parallel invocations produce ten branches, each with its own memory writes, none colliding with the others. Git's branching model gives you isolation without coordination, for free.

---

## The Honest Caveat

GitAgent ships with a built-in cron scheduler, accessible through its voice and web UI server. That scheduler runs inside a long-lived process — if you stop the server, scheduled jobs do not fire.

This is the right trade-off for interactive use cases: a developer running a personal assistant locally, an agent that needs to respond to voice commands, a setup where sub-minute scheduling matters.

For production workloads where reliability and cost matter — use an external scheduler to trigger single-shot runs. GitHub Actions, AWS EventBridge, GCP Cloud Scheduler, Render cron jobs — any of these will invoke the agent more reliably than an in-process scheduler, with no infrastructure to babysit, and with the agent's state persisting safely in git regardless.

---

## The Bigger Shift

The industry defaulted to always-on agents because it inherited the mental model of always-on services. But a service needs to be alive to handle requests. An agent needs to be capable — and capability lives in configuration files, memory, and learned skills, not in a running process.

When you store agent state in git, you decouple capability from availability. The agent does not need to be running to exist. It does not need a server to remember. It does not need uptime to be useful. It needs a repo.

This changes what it means to deploy an agent. There is no server to provision, no container to scale, no process to monitor. There is a repository. Fork it to create a new agent. Branch it to experiment. Tag it to pin a release. Push it to deploy. The entire operational model for AI agents collapses into git workflows that developers already know.

You do not need infrastructure to run intelligence. You need a repo and a reason to invoke it.

---

## Get Started

```bash
npm install -g @open-gitagent/gitagent
gitagent --prompt "Hello from a serverless agent"
```

- Website: [gitagent.sh](https://gitagent.sh)
- Repo: [github.com/open-gitagent/gitagent](https://github.com/open-gitagent/gitagent)
