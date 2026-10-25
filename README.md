# gitagent

A terminal agent in a single static binary. Its behavior lives in version-controlled files: `gitagent.yaml`, prompts, model profiles, skills and hooks. Any result can be traced to a commit.

It works the same way in every mode. One loop calls the model, runs its tool calls in a persistent shell, and records every turn. It runs one task in a folder, an interactive session, a task in a fresh clone of a repository, or a Terminal-Bench task through Harbor.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitagent/v2/scripts/install.sh | sh
```

This installs the binary for your system (Linux x86_64/arm64, macOS arm64/x86_64) into `~/.local/bin`; set `INSTALL_DIR` to change that. To build from source you need Rust (the version is pinned in `rust-toolchain.toml`):

```sh
cargo build --release            # ./target/release/gitagent
scripts/build-release.sh         # static Linux binaries (needs cargo-zigbuild on macOS)
```

## Use

Set the key for your model provider, then:

```sh
export ANTHROPIC_API_KEY=...

# One task in a folder; prints the agent's summary when done
gitagent -p "fix the failing test" --dir ./project --model anthropic:claude-opus-5-5

# Interactive: type tasks one per line; each continues the same session
gitagent --dir ./project --model anthropic:claude-opus-5-5

# Work in a fresh clone on a new branch, commit when done, push
GITHUB_TOKEN=... gitagent --repo https://github.com/org/app.git -p "fix the failing test" --push
```

In interactive mode, Ctrl-C stops the current task and Ctrl-D or `exit` quits.

### Models

`--model provider:model`, or `model:` in `gitagent.yaml`.

| Provider | Example | Key |
|---|---|---|
| Anthropic | `anthropic:claude-opus-5-5` | `ANTHROPIC_API_KEY` |
| OpenAI (Responses API) | `openai:<model>` | `OPENAI_API_KEY` |
| Google Gemini | `google:<model>` | `GEMINI_API_KEY` |
| Any OpenAI-compatible endpoint | `openrouter:<model>`, `ollama:<model>`, `vllm:<model>`, `<name>:<model>` | `{NAME}_API_KEY`; endpoint from `{NAME}_BASE_URL` (OpenRouter, Ollama and vLLM have defaults) |

The agent's shell never sees these keys.

### Repo mode

`--repo <url>` clones into `--dir` (default: `./<repo-name>`).

- **Branch:** it works on a new branch `gitagent/<task>-<id>` created from `--base` (default: the remote's default branch). `--branch <name>` continues an existing branch instead.
- **Commit:** when the agent finishes, it commits with the author from `gitagent.yaml`. Nothing is committed when nothing changed.
- **Push:** only with `--push`.
- **Safety:** it never commits to or pushes the base branch, and never force-pushes.
- **Token:** `GITAGENT_GIT_TOKEN` or `GITHUB_TOKEN` is passed to git through a helper. It is never written to the repository, the logs or the events.

### Parallel work

With `runtime: {parallel_tasks: true}` in agent.yaml, the agent gets a
`plan` tool: it writes phases of tasks, every task in a phase runs at the
same time as its own sub-agent (fresh context, own shell), a later phase
sees the earlier results, and one more sub-agent reconciles everything into
the answer. `runtime.max_parallel` (default 4) says how many run at once.
Use it for work that splits into similar pieces: many files to read, many
items to check. It costs tokens in proportion, so give it scoped tasks.

```yaml
runtime:
  parallel_tasks: true
  max_parallel: 8
```

### Schedules

`schedules/*.yaml` in the agent directory are prompts the agent runs on a
timetable, as in gitagent 1.x:

```yaml
id: daily-triage
description: Morning ticket triage
cron: "0 8 * * 1-5"      # minute hour day month weekday, local time (six fields add seconds)
prompt: |
  Review the open tickets and save a summary to memory.
mode: repeat             # or once; `runAt: 2026-11-01T09:00:00Z` runs one time at a moment
enabled: true
```

```sh
gitagent schedule list -d ./project                      # with each one's next run
gitagent schedule add nightly --cron "0 2 * * *" --prompt "Tidy the notes." -d ./project
gitagent schedule run nightly -d ./project               # once, now
gitagent schedule start -d ./project                     # keep running; fires each schedule when due
gitagent schedule disable nightly -d ./project           # also enable, remove
```

`start` runs one job at a time, each as a fresh conversation, logs it to
`.gitagent/schedule-logs/<id>.jsonl` and records `lastRunAt` and
`lastResult` in the schedule's file (a `once` schedule is then turned off).
Like 1.x's in-process scheduler, jobs fire only while `start` is running;
for anything that must not be missed, have an external scheduler call
`gitagent schedule run <id>`.

## As a library

The `gitagent-sdk` crate offers a library interface over the same loop: `query(prompt, AgentOptions)` streams `system` / `assistant` / `user` / `result` messages, tools appear under the `.claude/` tool names (`Bash`, `Read`, `Edit`, …), and hooks are callbacks that receive `PreToolUse` / `PostToolUse` input and answer with `permissionDecision`, `updatedInput` or `updatedToolOutput`.

```rust
use gitagent_sdk::{query, AgentOptions, AgentDefinition, HookEvent, HookMatcher, HookOutput, Message};

let mut q = query("Fix the failing test.", AgentOptions {
    model: Some("claude-opus-5-5".into()),
    allowed_tools: vec!["Bash".into(), "Read".into(), "Edit".into()],
    max_turns: Some(40),
    hooks: vec![HookMatcher::new(HookEvent::PreToolUse, Some("Bash"), |input| async move {
        if input.tool_input["command"].as_str().unwrap_or("").contains("rm -rf") {
            HookOutput::deny("no rm -rf")
        } else {
            HookOutput::allow()
        }
    })],
    ..Default::default()
}).await?;
while let Some(message) = q.next().await {
    if let Message::Result { result, total_cost_usd, .. } = &message {
        println!("{result:?} (${total_cost_usd:.2})");
    }
}
```

The same shape is available from the command line: `gitagent -p "..." --output-format stream-json` prints one message per line, and `--max-turns`, `--allowed-tools`, `--disallowed-tools` and `--append-system-prompt` take the same values as `AgentOptions`.

## Configuration

gitagent reads `gitagent.yaml` from `--config`, else the project folder (`gitagent.yaml` or `.gitagent/gitagent.yaml`), else `~/.gitagent/gitagent.yaml`. The other files are looked up one by one in the same places (the config's folder, the project's `.gitagent/`, then `~/.gitagent/`, then the project's and home `.claude/`), and fall back to the built-in versions:

| Path | What it is |
|---|---|
| `gitagent.yaml` | Model, turn and time limits, profile, sandbox, skills, hooks, learning, repo-mode settings |
| `prompts/system.md`, `prompts/rules.md` | The system prompt; `rules.md` is appended to it |
| `profiles/<family>.yaml` | Per-model-family settings: tools, effort, output shaping, caching, and the optional features below |
| `skills/<name>/SKILL.md` | Instructions the agent can load on demand; their names and descriptions are listed in the system prompt |
| `hooks/pre_tool_use`, `hooks/post_tool_use` | Scripts that receive each tool call (and its output) as JSON and answer `allow`, `block` or `modify` |
| `prompts/lessons.md` | Learned lessons appended to the system prompt; unlike the other files, every layer's lessons are added up |
| `agents/<name>.md` | Sub-agent definitions (`name`, `description`, `tools`, `disallowedTools`, `maxTurns`, `model`, `effort`; the body is the prompt), when the profile enables sub-agents |
| `.claude/agents/`, `.claude/skills/`, `.claude/settings.json` | An existing `.claude/` project layout, read as it is after the layers above: sub-agents and skills, and `PreToolUse` / `PostToolUse` hooks with their matchers, stdin JSON, exit codes and `hookSpecificOutput` answers (tool and argument names are translated both ways) |

`profile: auto` picks `anthropic`, `openai`, `google` or `compat` from the model's provider. Every behavior that changes results is a switch in the profile. Some features stay off until measured: the environment snapshot, the stop check, the time budget, the loop detector and hiding old tool output. Context compaction is on.

With `learning: online`, gitagent learns from each task it completes. The model reads the run's trajectory and proposes general lessons, which are saved in the project's `.gitagent/prompts/lessons.md`. A lesson is used only once two different tasks support it. Review the file like code: edit or delete lines freely. In repo mode the lessons are committed separately on the branch. `--bench` never learns and never loads project lessons.

### Output, records and events

- **Records:** every run writes `trajectory.json` (Harbor's ATIF format), `counters.json` and the full output of every command (`out/<n>.log`) to `--out-dir`. They are updated after every turn.
- **Events:** `--events jsonl` prints one JSON object per line on stdout: `turn_start`, `model_text`, `tool_call`, `tool_result`, `stop_check`, `guard`, `turn_end`, `repo`, `run_end`.
- **Telemetry:** set `OTEL_EXPORTER_OTLP_ENDPOINT` to export traces and metrics (OTLP over HTTP/JSON). `OTEL_EXPORTER_OTLP_HEADERS` and `OTEL_SERVICE_NAME` are honoured.

### Sandbox

On Linux, outside benchmark runs, the agent's shell runs under Landlock: it may write only to the working folder, the output folder, the temp folders and `~/.cache`. A seccomp filter blocks system calls such as `mount`, loading kernel modules and `reboot`. Set `sandbox: off` in `gitagent.yaml` to disable it, or `sandbox: on` to require it. Writing under `/tests` is refused in every mode.

## Terminal-Bench with Harbor

```sh
scripts/build-release.sh
PYTHONPATH=$PWD harbor run -d terminal-bench/terminal-bench-2-1 \
  -a gitagent_harbor.agent:GitAgent -m anthropic/claude-opus-5-5 \
  --env-file .env --ak timeout_s=840 -k 3 -n 8 -o bench/jobs
```

The shim (`gitagent_harbor/agent.py`):

- **Install:** uploads the static binary for the container's CPU and runs it with `--bench`, which uses only the built-in configuration.
- **After the run:** keeps the agent's shells open, so services it started are still up when the tests run, and returns once the agent is done.
- **Shim settings** (via `--ak`): `profile=<name or path>`, `timeout_s=<seconds>`.

`bench/RESULTS.md` records every run, and `docs/INTEGRITY.md` lists the rules that keep results honest. `scripts/integrity-audit.sh` checks them.

`python3 scripts/dashboard.py` serves a local dashboard over `bench/jobs` at http://127.0.0.1:8731, refreshing live while runs are going: jobs and trials (agent, oracle and baseline runs kept apart), accuracy per task, why each trial failed, every counter with its meaning, the guard and loop events in a trajectory, each trial's commands, outputs and log, the lesson store, the task catalog with difficulty and limits, and the feature switches of every profile. It reads only gitagent's own files, each trial's reward and the tasks' task.toml metadata.

### Learning from finished runs

`gitagent learn` turns finished Harbor trials into general lessons for the system prompt. It reads only gitagent's own trajectory and the reward of each trial, asks the model for lesson changes, rejects any that carry task details, and rewrites `prompts/lessons.md`. A lesson is shown to the agent only once attempts at two different tasks support it.

```sh
gitagent learn --model anthropic:claude-opus-5-5 --from bench/jobs/<job> [--from …] [--dry-run]
```

It learns only from tasks in the `dev` list of `bench/split.json`. Review the diff of `prompts/lessons.md` before committing it; set `lessons: false` in a profile to leave lessons out.

## Development

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

The tests need no network or keys: a scripted provider replays model replies from YAML, and the provider adapters are tested against local mock servers.

- `BLUEPRINT.md`: what is built and why.
- `TASKS.md`: the milestones.
- `docs/DEVIATIONS.md`: where the implementation differs from the blueprint, and why.
