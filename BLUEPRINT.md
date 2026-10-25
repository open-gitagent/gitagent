# gitagent 3 — Blueprint

This document says **what** gitagent is built to do and **why** each part exists. The README says how to use it; [docs/HOW-IT-WORKS.md](./docs/HOW-IT-WORKS.md) gives the exact mechanics, with numbers and formats.

---

## 0. Goal in one paragraph

gitagent 3 is a single static Rust binary that runs **agents defined as git repositories**. An agent is a folder of plain files (identity, rules, memory, skills, tools, hooks, schedules, sub-agents), so its behaviour is versioned, reviewed and diffed like code. One loop runs every agent, the same way in a terminal, behind an SDK, in a fresh clone of a repository, on a timetable, or inside a benchmark container. That loop is built to finish real work: it shapes what the model reads, keeps the provider's cache warm, guards against loops and running out of time, and refuses "done" until the work has been verified. The proof is public: 93.03% on Terminal-Bench 2.1 with Claude Opus 5.5, above every published harness on that benchmark.

---

## 1. Requirements (every design choice traces to one)

**The agent**

| ID | Requirement |
|---|---|
| A1 | An agent is a folder of version-controlled files; every behaviour traces to a commit |
| A2 | Agent folders written for the TypeScript gitagent (1.x and 2.x) and in the OpenGAP format run unchanged |
| A3 | Usable from a terminal, from Python, TypeScript or any other language, from a repository clone, and on a schedule |
| A4 | Safe to run on a real machine: a sandboxed shell, hooks that can block any action, approvals by a person, an audit log |
| A5 | Large jobs can be split into parallel pieces and reconciled into one answer |
| A6 | Works with Anthropic, OpenAI, Google and any OpenAI-compatible endpoint, with fallbacks |

**The loop**

| ID | Requirement |
|---|---|
| L1 | A higher Terminal-Bench 2.1 pass rate than any published harness on the same model |
| L2 | Median time per task at or below other harnesses; timeouts rare |
| L3 | Most input tokens served from the provider's cache on long tasks |
| L4 | Handles REPLs, servers, long builds and interrupts |
| L5 | Never accepts "done" without a verification after the last change |
| L6 | Runs in any Linux container with no dependencies |
| L7 | Every tunable behaviour is a switch in a YAML profile, measured on its own |
| L8 | Every feature emits counters; every run writes a complete trajectory, every turn |
| L9 | Benchmark integrity: no reading or writing of hidden tests, no routing on task names, no network but the model API, no learning during a scored run |

---

## 2. Why Harbor

gitagent is measured on Terminal-Bench 2.1, and Terminal-Bench runs through **Harbor**, an open framework for running agents against containerised tasks. Harbor matters for four reasons.

1. **It is the same yardstick as the published results.** The leaderboards for Terminal-Bench 2.1 were produced with Harbor: the same task images, the same time limits, the same hidden tests, the same scoring. Running gitagent through it is what makes 93.03% comparable with the published 87.6% for Terminus 2 on the same model, rather than a number from a home-made runner.
2. **Grading is independent of the agent.** Harbor starts each task's container, hands the agent the instruction and the time limit, and when the agent has finished it runs the task's hidden tests itself and records the reward. gitagent never sees, reads or writes the tests. The result is a pass or a fail decided outside the agent, which is the only kind worth publishing.
3. **It fits a static binary.** Harbor's "installed agent" model uploads the agent into the task container and runs it there. gitagent is one binary with no runtime, so installing it is a single file copy, and nothing in the container has to change. The shim that does this, `gitagent_harbor/agent.py`, is about two hundred lines.
4. **It produces evidence others can check.** Harbor runs many trials in parallel, resumes trials that failed for reasons outside the agent (a revoked key, a full Docker network), keeps every trajectory and verifier log, and uploads a job publicly. The headline run's nine jobs, one per time limit, are public, so anyone can read every trial behind the number.

Harbor also hosts other benchmarks with the same task shape, SWE-bench Verified and SWE-bench Pro among them, so the same shim can be pointed at them without changes to the agent.

**How gitagent runs under Harbor.** The shim uploads the binary for the container's CPU to `/opt/gitagent/`, then runs `gitagent --bench --instruction-file … --model … --profile … --linger`. `--bench` makes the run depend only on files committed in the repository: no project or home configuration, no memory, no learning, no telemetry, repo mode refused, writes under `/tests` refused. `--linger` keeps the agent's shells open after it finishes, so a server it started is still up when the tests connect. The trajectory and counters are written to Harbor's log folder every turn, so even a timed-out trial keeps its record. A test, `bench_prompt_and_tools_match_the_published_run`, fails if the prompt or tool list sent to the model in `--bench` ever changes, so features built for agent folders can never leak into a scored run.

---

## 3. Architecture

```
   console · -p · --repo        Python · TypeScript · any language            Harbor
   gitagent schedule start            (gitagent --ipc, JSON lines)        (gitagent --bench)
              │                                  │                               │
              └──────────────────┬───────────────┴───────────────┬───────────────┘
                                 ▼                               ▼
                 ┌──────────────────────────────────────────────────────────┐
                 │  loader: agent folder + config layers ──▶ one AgentSpec   │
                 │    agent.yaml · SOUL/RULES/DUTIES · skills · tools ·     │
                 │    hooks · memory · knowledge · workflows · schedules ·  │
                 │    agents · plugins · config/<env> · compliance          │
                 ├──────────────────────────────────────────────────────────┤
                 │  core: THE LOOP                                          │      Model APIs
                 │    provider ── native tool calls, cache markers ─────────┼────▶ Anthropic, OpenAI,
                 │    tools ── shell, files, edits, memory, skill,          │      Google, compatible
                 │             subagent, plan, workflow, YAML, MCP          │
                 │    terminal ── persistent PTY shells                     │
                 │    shaper ── output to a budget, spill files             │
                 │    guards ── stop check, time budget, loop detector      │
                 │    context ── masking, compaction                        │
                 │    hooks · approvals · audit                             │
                 ├──────────────────────────────────────────────────────────┤
                 │  record: trajectory + counters every turn, events        │
                 │  sandbox: Landlock + seccomp around the agent's shell    │
                 └──────────────────────────────────────────────────────────┘
```

### One run, start to finish

1. **Start.** An entry point starts the loop with an instruction: the console, `-p`, repo mode, an IPC client, the scheduler, or Harbor.
2. **Load.** The loader reads the agent folder and the configuration layers into one spec and assembles the system prompt in a fixed order: identity, working method, lessons, skills, sub-agents, workflows, knowledge, examples, compliance, memory, bootstrap. In `--bench` only the built-ins apply, and the prompt is byte-identical across tasks so the cache holds.
3. **Snapshot.** One probe, at most 15 seconds, collects the working directory, the system, the tools present and the directory listing, and puts it in the first user message, never the system prompt.
4. **Model call.** The provider adapter sends the request with native tool calls and cache markers; malformed calls get one repair request.
5. **Dispatch.** Each tool call passes the hooks and, when the agent's rules require it, a person's approval, then runs. Shell commands run in a persistent shell with an end marker; sub-agent and plan calls run concurrently.
6. **Shape.** Every result is cut to a token budget, with similar lines collapsed and the full output saved to a spill file.
7. **Guard.** The time left is shown on every result, a reminder fires at 70% of the budget and a final call at 90%; a call repeated with the same result three times gets a change-approach warning.
8. **Context.** Old results are cleared in batches, through the provider's context editing where it exists; near the window's end the history becomes a structured summary with the task repeated verbatim.
9. **Stop check.** `task_complete` is accepted only when a verification ran after the last change.
10. **Record.** The trajectory step and counters are written after every turn.

Steps 4 to 9 repeat until the stop check accepts, the agent replies in conversation mode, or a limit is hit.

---

## 4. Design decisions

| # | Decision | Why |
|---|---|---|
| D1 | One static Rust binary | No runtime to install anywhere; fast start; direct PTY access; memory safety; a sandbox from inside the process |
| D2 | The agent is a folder of files | Behaviour is versioned and reviewable; forking, branching and diffing an agent are git operations (A1) |
| D3 | Accept the TypeScript gitagent's and OpenGAP's formats, warn about old spellings | Existing agents run unchanged; `validate` points to the newer form instead of failing (A2) |
| D4 | One loop for every entry point | A fix or a measurement applies everywhere; the benchmark result describes the product |
| D5 | A single linear, append-only history | Keeps the provider's cache valid; the gains are in the loop, not in agent graphs |
| D6 | Native tool calls with a text fallback and one repair | Reliable calls on every provider, open-weight models included |
| D7 | Persistent PTY shells with end markers and background sessions | No guessed waits; REPLs and servers work; Ctrl-C reaches only the running job |
| D8 | A small tool set, edit tool chosen per model family | A short cached prefix and fewer wrong choices; `apply_patch` for GPT, `str_replace` for Claude |
| D9 | The environment snapshot in the first message | Saves the first turns of looking around without touching the cached system prompt |
| D10 | An output shaper with spill files | The largest token cost, cut without losing anything |
| D11 | Batched masking, server-side where possible, and structured compaction near the limit | About half the cost at the same solve rate, with the cache and thinking blocks intact |
| D12 | An evidence-based stop check that never rewrites the answer | Premature "done" is the most common failure; critics that rewrite answers make results worse |
| D13 | A time budget and a loop detector | Timeouts are failures; loops waste the budget |
| D14 | Per-model-family profiles in YAML, every switch measured on its own | Settings do not transfer across providers; a switch that does not help is turned off |
| D15 | Thin provider adapters of our own | Byte-level control of requests, which cache hits depend on |
| D16 | Counters and a trajectory written every turn | A feature that never fires shows up; a timed-out run keeps its record |
| D17 | Hooks fail open, compliance hooks fail closed | Existing hook scripts behave as before; rules that must hold always do |
| D18 | Clients talk to the binary over JSON lines on stdin and stdout, one process per session | Any language can drive an agent; the agent logic exists once; a crash in a client cannot corrupt a run |
| D19 | Parallel work as a `plan` tool over sub-agents, with a reconciling step, opt-in per agent | Large jobs finish faster and more consistently; bounded phases and tasks keep the cost predictable |
| D20 | Schedules run in-process while `gitagent schedule start` runs | The same behaviour as before; anything that must not be missed calls `gitagent schedule run` from an external scheduler |
| D21 | The terminal is the interface | One console with tool lines, approvals, a task board and command suggestions; no web server to secure |
| D22 | Landlock and seccomp around the agent's shell, not around gitagent | The agent cannot write outside its folders, while gitagent can still record |
| D23 | `--bench` reads only committed files, and a test pins its prompt and tools | Product features cannot change a scored run by accident |

**Left out on purpose:** critics that rewrite answers; best-of-N on benchmarks; memory or learning during a scored run; automated rewriting of the harness; masking every turn; third-party multi-provider frameworks; a web interface; voice; remote sandboxes.

---

## 5. Product surface

| Capability | How | Notes |
|---|---|---|
| Console | `gitagent -d <agent>` | Welcome box, `$` tool lines, a thinking indicator, approvals inline, `/` command suggestions, a task board for plans, `/help` guide |
| One task | `gitagent -p "…" -d <folder>` | Prints the agent's summary; exit 3 on a provider failure |
| Repo mode | `gitagent --repo <url> -p "…" [--push]` | Clones (or reuses a clone), works on a new branch, commits when done; never pushes the base branch or force-pushes |
| Agent folders | `agent.yaml`, `SOUL.md`, `RULES.md`, `DUTIES.md`, `memory/`, `skills/`, `tools/`, `hooks/`, `agents/`, `workflows/`, `schedules/`, `knowledge/`, `examples/`, `plugins/`, `config/` | `gitagent init` creates one; `gitagent validate` checks one; `extends` and `dependencies` compose them (`gitagent install`) |
| Memory | `memory/` with layers and line limits | Each save is a git commit |
| Hooks | `hooks/hooks.yaml`, eight events | Scripts answer allow, block or modify |
| Compliance | `compliance:` in `agent.yaml` | Human in the loop, escalation triggers, kill switch, segregation of duties, spending limits, audit log |
| Sub-agents and parallel work | `agents/`, the `subagent` tool, `runtime.parallel_tasks` | Phases of tasks run at the same time, then reconciled |
| Workflows | `workflows/*.yaml`, `@<name>` | Ordered steps with conditions, retries and approvals |
| Schedules | `schedules/*.yaml`, `gitagent schedule …` | Cron or a one-time moment; each job logged and recorded in its file |
| MCP | `mcp_servers:` | stdio, streamable HTTP and SSE servers; their tools join the agent's |
| Plugins | `plugins/`, `gitagent plugin …` | Tools, skills, hooks and prompt text from a folder or a git URL |
| SDKs | Python, TypeScript, any language over `gitagent --ipc`; a Rust crate in-process | The TypeScript gitagent's `query()` API |
| Benchmark | `gitagent --bench` under Harbor | §2 |
| Learning | `gitagent learn`, `learning: online` | Lessons from finished runs, audited, shown only when two tasks agree; never during a scored run |
| Telemetry | `OTEL_EXPORTER_OTLP_ENDPOINT` | Traces and metrics over OTLP; off by default |

---

## 6. Code layout

A Cargo workspace; `unsafe` is forbidden everywhere.

```
gitagent/
├─ crates/
│  ├─ cli/        the binary: flags, console, line editor, IPC server, subcommands
│  ├─ core/       the loop, sub-agents, plan, workflows, approvals, audit
│  ├─ loader/     agent folders and config layers → AgentSpec; validate
│  ├─ provider/   Anthropic, OpenAI, Google, compatible, scripted (tests)
│  ├─ tools/      built-in tools, YAML tools, edit tools
│  ├─ terminal/   persistent PTY shells, end markers, background sessions
│  ├─ shaper/     output shaping and spill files
│  ├─ context/    masking and compaction
│  ├─ guards/     stop check, time budget, loop detector
│  ├─ hooks/      hook scripts and their protocol
│  ├─ mcp/        the MCP client
│  ├─ record/     trajectory, counters, events
│  ├─ repo/       clone, branch, commit, push
│  ├─ sandbox/    Landlock and seccomp
│  ├─ learn/      offline learning of lessons
│  └─ sdk/        the in-process Rust library
├─ clients/       python/ and typescript/: thin clients over --ipc
├─ gitagent_harbor/  the Harbor shim
├─ profiles/      anthropic, openai, google, compat
├─ prompts/       system, conversation, lessons, learner
├─ bench/         benchmark profiles, task split, job records
├─ scripts/       install, release builds, integrity audit, benchmark runs, dashboard
└─ docs/          how it works, integrity, learning
```

The Provider interface every adapter implements:

```rust
pub trait Provider: Send + Sync {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, ProviderError>;
    fn family(&self) -> Family;   // Anthropic | OpenAI | Google | Compat
}
```

Dependencies are kept few: `tokio`, `reqwest` with rustls, `serde`, `serde_json`, a YAML parser, `clap`, `regex`, `portable-pty`, `anyhow`, `thiserror`, `async-trait`, and `landlock` with a seccomp filter for the sandbox. Token counts are estimated at four bytes per token instead of linking a tokenizer. Release builds are static musl binaries for x86_64 and aarch64 Linux, plus macOS.

---

## 7. Configuration surfaces

- **`agent.yaml`**, in the agent folder: what the agent is. Model with fallbacks and constraints, tools, runtime limits, parallel tasks, skills, sub-agents, MCP servers, plugins, compliance, `extends` and `dependencies`. The README's Architecture section shows a full example.
- **`gitagent.yaml`**, in the project or `~/.gitagent/`: how the binary runs. Default model, turn and time limits, profile, sandbox, skills and hooks on or off, learning, memory, MCP servers, repo-mode settings.
- **`profiles/<family>.yaml`**: how the loop behaves for one model family. Tool calling, edit tool, effort, output budget, context and cache settings, the guards, the stop check's patterns, and every optional feature as a switch. `profile: auto` picks the family from the model.
- **`config/<env>.yaml`**, in the agent folder: per-environment overrides selected with `--env`: model, temperature, timeout, environment variables for tools and hooks.

---

## 8. Interfaces

- **CLI.** `-p`, `-d`, `-m`, `-e`, `--repo`, `--push`, `--base`, `--branch`, `--bench`, `--instruction-file`, `--profile`, `--config`, `--out-dir`, `--events jsonl`, `--output-format stream-json`, `--ipc`, `--workflow`, and the subcommands `init`, `validate`, `install`, `plugin`, `schedule`, `learn`.
- **Events** (`--events jsonl`): one JSON object per line: `turn_start`, `model_text`, `tool_call`, `tool_result`, `stop_check`, `guard`, `plan`, `turn_end`, `repo`, `run_end`.
- **IPC** (`gitagent --ipc`): JSON lines on stdin and stdout. The client sends `initialize`, `user`, `steer`, `abort`, `kill`, `callback_response`, `end`; the binary sends `ready`, the conversation's messages, `callback_request` for hooks, client tools and approvals, `turn_end` and `session_end`. Sessions can be resumed across processes.
- **Harbor shim** (`gitagent_harbor/agent.py`): installs the binary, runs `--bench --linger`, and returns the trajectory and counters to Harbor.

---

## 9. Observability

Counters are written to `counters.json` after every turn and exported with telemetry when it is on. They cover the loop (`turns`, `tool_calls`, `malformed_calls`, `repairs`, `refusals`, `fallback_turns`), the shell (`exec_timeouts`, `background_sessions`, `interrupts`, `shell_restarts`), shaping and context (`shaped_bytes_in`, `spill_reads`, `masks_applied`, `mask_checkpoints`, `compactions`, `cache_read_tokens`, `cache_write_tokens`), the guards (`stop_accepts`, `stop_rejections`, `loop_triggers`, `protected_writes_blocked`), hooks and compliance (`hook_blocks`, `hook_modifies`, `approvals_granted`, `approvals_denied`, `compliance_blocks`), memory, sub-agents and plans (`subagent_calls`, `plan_tasks`, `plan_failed_tasks`), workflows and MCP.

A feature whose counters stay at zero across a measurement run is either broken or useless, and is investigated before it is kept.

---

## 10. Testing

- **Unit tests** next to the code: shaping, end markers, the stop check, the loop hash, masking, profile and agent-folder loading, cron parsing, the line editor.
- **End-to-end tests** with the scripted provider: model replies replayed from YAML drive the real loop against a real PTY in a temporary folder, covering every entry point, IPC, hooks, approvals, sub-agents, plans, schedules and repo mode. No network and no keys.
- **The benchmark guard**: the prompt and tools sent in `--bench` must match the published fixtures byte for byte.
- **Client tests**: the Python client's unit tests, the TypeScript client's type check.
- **Benchmark measurement**: every change that can affect results is measured with Harbor, three or more trials per task, compared task by task with the previous version, on tasks kept apart from the ones used to score.

---

## 11. Open questions for the owner

1. A SWE-bench Verified entry: run it, and with which model?
2. Publishing the clients: package names and registries for Python and TypeScript.
3. Releasing 3.0.0: the squash-merge into `main`, the tag and the release notes.
4. Token-by-token streaming over IPC, which version 1 of the protocol does not have.
5. Running a plugin's `entry` JavaScript, which gitagent 3 does not do.
