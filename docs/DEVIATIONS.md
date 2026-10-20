# Deviations from BLUEPRINT

Where an upstream API differs from the spec, we follow upstream and note it here.

## Harbor CLI (harbor 0.24.0)

The originally planned run command used flags that Harbor 0.24.0 does not have:

| Planned | Harbor 0.24.0 |
|---|---|
| `--agent-import-path harbor.agent:GitAgent` | `-a harbor.agent:GitAgent` (`--agent` takes a built-in name or a `module.path:ClassName`) |
| `--task-ids <ids…>` | `-i <task>` repeated once per task (`--include-task-name`); `-x` excludes |
| `-k 3`, `--n-concurrent 8` | unchanged (`--n-attempts`, `--n-concurrent` / `-n`) |

Task names in the dataset carry a `terminal-bench/` prefix (e.g. `terminal-bench/kv-store-grpc`); `bench/split.json` stores the full names.

Working form (see the shim section below for the module path):

```
PYTHONPATH=$PWD harbor run -d terminal-bench/terminal-bench-2-1 -a gitagent_harbor.agent:GitAgent \
  -m <provider/model> --ak timeout_s=<seconds> -k 3 -n 8 -o bench/jobs \
  $(jq -r '.dev[] | "-i " + .' bench/split.json)
```

## Harbor shim location (BLUEPRINT §5, §7)

BLUEPRINT puts the shim at `harbor/agent.py`. A local top-level package named `harbor` would shadow the installed `harbor` package that the shim must import from (`harbor.agents.installed.base`). The shim lives at `gitagent_harbor/agent.py` instead and is loaded with `PYTHONPATH=$PWD harbor run -a gitagent_harbor.agent:GitAgent …`. Harbor does not put the working directory on `sys.path` by itself.

## Trajectory and log paths (BLUEPRINT §7)

BLUEPRINT says the shim copies `/tmp/gitagent/trajectory.atif.json`, `out/` and counters back. Harbor already syncs `/logs/agent/` in the container to the trial's `agent/` dir (bind mount or download after the run, including after a timeout), and backfills token usage from `agent/trajectory.json`. In `--bench`, the binary writes `trajectory.json`, `counters.json` and the spill dir under `/logs/agent/` directly, so no copy step can be lost on a timeout. Outside `--bench` the paths stay configurable.

## ATIF version

Harbor 0.24.0 validates ATIF `ATIF-v1.0`…`ATIF-v1.8`; we write `ATIF-v1.8`. Every model is `extra="forbid"`, so anything of ours (counters, guard events) goes in the `extra` fields, never as new keys. `step_id` must run 1, 2, 3, … and every observation result's `source_call_id` must match a tool call in the same step.

## Time budget (TASKS M4)

TASKS M4 says to read Harbor's task timeout from the environment or flags. Harbor 0.24.0 does not pass it to installed agents; it only enforces it from outside with `asyncio.wait_for`. The binary takes `--timeout-s`, and the shim fills it in from a shim kwarg (`--ak timeout_s=…`). Without it the binary uses `timeout_s` from `gitagent.yaml` (1800 s); if Harbor's limit is shorter, Harbor stops the run, and the trajectory written up to the last turn is kept.

## API keys

Harbor does not forward host environment variables into the container. The shim passes the provider key explicitly via `env=` on exec (from Harbor's resolved env, `--ae`, or the host environment).

## Profile fields added (BLUEPRINT §6)

Profiles also set `max_output_tokens` (providers require an output limit per call) and `context_window` (compaction in M4 needs to know how much room is left). Every profile field is required, so a profile always states its values.

## Model string from Harbor

Harbor passes models as `provider/model`; the shim converts this to the binary's `provider:model` form.

## Scripted provider

`--model scripted:<fixture.yaml>` replays a fixed list of tool calls. It is used by the integration tests and for Harbor dry runs without a model API (`-m scripted/<name> --ak script=bench/fixtures/smoke.yaml`). It never decides anything from the task.

## Anthropic: request surface (current Claude models)

- **No temperature.** Current Claude models reject sampling parameters with a 400, so `profiles/anthropic.yaml` sets none (BLUEPRINT §6 shows `temperature: 1.0`).
- **Effort and thinking.** Thinking is adaptive and always on; `output_config.effort` is the control and is always sent explicitly (some models default to `medium`). Effort accepts `low`, `medium`, `high`, `xhigh`, `max`.
- **Output limit.** `max_output_tokens: 64000`, because thinking counts toward it. Responses are streamed so long turns do not hit HTTP timeouts.
- **Refusals.** A safety-classifier refusal ends the run with outcome `refused` (counter `refusals`). The profile enables the API's server-side refusal fallback (`fallbacks: "default"`), which re-runs a refused turn on a model Anthropic picks for that refusal category. Turns served by another model are counted in `fallback_turns`. Set `anthropic.refusal_fallback: false` for strict same-model comparisons.

## Anthropic: thinking blocks bind the history (affects M4 masking and compaction)

Thinking blocks are replayed verbatim and are bound to the exact conversation before them. Editing, removing or rewriting an earlier turn invalidates every later thinking block, and accounts created on or after 2026-08-31 get a 400 for it. The adapter sets `thinking.block_binding.prefix_mismatch_behavior` explicitly (profile `anthropic.thinking_binding`, default `error`) so any accidental edit fails loudly; the M1 loop is append-only.

BLUEPRINT §5's batched masking replaces old tool results with stubs in place. That is such an edit. Options for M4, to be measured on the dev split:

1. Anthropic's server-side tool-result clearing (context editing), which does not count as an edit.
2. Keep client-side masking at the batched checkpoints only, and send `drop_block` on those requests. Thinking blocks after the edit are dropped once per checkpoint.
3. Compaction that replaces the whole history with a summary and replays no earlier thinking blocks, as BLUEPRINT's structured compaction already does.

Time-budget nudges (M4) must be appended (a mid-conversation `role: "system"` message or a text block after the tool results), never injected into an earlier turn.

## HTTP client

reqwest 0.12 with `rustls-tls` (ring plus bundled Mozilla root certificates) instead of 0.13, whose default TLS uses aws-lc and the system certificate store. Many task containers have no CA bundle; with bundled roots the static binary reaches the API from a container without `/etc/ssl` (checked in `busybox`).

## Processes left running after the agent ends (Harbor shim, `--linger`)

Harbor runs the task's tests after the agent process exits. Terminus 2 runs inside tmux, whose server keeps running in the container, so a server the agent started with `cmd &` is still up during the tests. A plain process would stop its shells on exit, and the terminal hang-up kills such jobs. Only `nohup`-style detached processes would survive.

So in benchmark runs the shim starts gitagent detached with `--linger`. After the run, gitagent writes `done` (its exit code) next to the trajectory and keeps every shell open, draining output, until the shells exit or the container is removed. The shim returns as soon as `done` appears, so Harbor's agent timing ends with the agent's work. Outside the shim, gitagent still stops everything it started when it exits.

## Output shaper (BLUEPRINT §5)

- **Spill files** go to `{out-dir}/out/{n}.log`, next to the trajectory, so Harbor collects them. `spill_dir` was removed from `gitagent.yaml`.
- **Collapsing** keeps the first and the last line of a run of similar lines (the blueprint keeps only the first): the last line of a progress or build run is usually the one that matters. Any run of digits counts as one digit when comparing lines. `collapse_min_run: 0` turns collapsing off.
- **Token estimates** use about four bytes per token instead of a tokenizer library, to keep the binary small. The cap is approximate by design.
- **When no whole line fits** the budget (minified files, huge single lines), the shaper cuts characters from the middle instead.
- **Status line:** it names the spill file only when something was hidden, so short outputs stay short. The time-left part of the blueprint's footer comes with the time budget (M4).

## OpenAI: Responses API, stateless

- **Endpoint:** the adapter uses the Responses API (`POST /v1/responses`), not Chat Completions, because reasoning items can be carried across turns there. OpenAI-compatible endpoints for open-weight models (the `compat` family, M5) will use Chat Completions.
- **No stored state:** requests use `store: false` and replay the whole history every turn. Output items, including encrypted reasoning, go back unchanged, as OpenAI asks for reasoning models with tool calls. `include: ["reasoning.encrypted_content"]` is sent for older models; newer ones include it by default.
- **Prefix and effort:** the system prompt goes in `instructions` and stays byte-identical. Effort goes in `reasoning.effort`. Reasoning summaries (`openai.reasoning_summary: auto`) are recorded as `reasoning_content` in the trajectory.
- **Cache key:** every task sends the same `prompt_cache_key` (`openai.prompt_cache_key`), so requests that share the system prompt and tools are routed to the same cache.
- **No temperature** in `profiles/openai.yaml` (reasoning models do not take sampling parameters).
- **Errors:** quota and billing 429s are fatal, not retried; `context_length_exceeded` is reported as context overflow.
- **Not live-tested yet:** no OpenAI key so far. Request building, stream parsing and error handling are tested against a local mock server.

## M3: snapshot and stop check are off by default

Both exist and are tested, but `profiles/*.yaml` keep them off (`snapshot: false`, `stop.enabled: false`) until a dev-split comparison shows they help. `bench/profiles/anthropic-m3.yaml` switches them on for that comparison; the Harbor shim uploads a profile file given as `--ak profile=<path>`.

- **Snapshot** runs as one `sh -c` probe in its own process with a 15 s limit, not in the agent's shell, so it leaves no shell state and no command history. Memory comes from `/proc/meminfo`, because many task images lack `free`. On any failure the snapshot is left out and `snapshot_failed` is counted.
- **Stop check:** edits are `write_file` calls plus commands matching `stop.edit_patterns`. Redirections to `/dev/null` and between file descriptors are ignored. Verification is a *finished* command matching `stop.verify_patterns`. A command that both edits and verifies counts as verified. The rejection message names the unverified change and repeats the original task. The patterns are deliberately broad on the verify side (they include running programs and `git log`/`status`), so the check only catches completions with no evidence at all; tune them on the dev split.

## M4: time budget, loop detector, masking, compaction

All are profile switches. `bench/profiles/anthropic-m4.yaml` turns them on for measurement. In `profiles/*.yaml` everything is off except compaction.

- **Reminders are appended, never injected.** Time reminders and loop warnings are added as a text block after the tool results of the turn, so earlier turns never change. Changed earlier turns would reset the prompt cache and invalidate Claude's thinking blocks. They also appear as user steps in the trajectory.
- **Time budget:** every command result shows `· N min left`. A reminder fires at `nudge_at` and a final call at `force_stop_at`. After the final call the stop check accepts `task_complete` without evidence, because there is no time left for a rejection to help.
- **Loop detector:** compares tool name, arguments and the first and last 20 output lines, with digit runs normalized. The effort increase after a warning (`loop_effort_bump`) is off by default: on Anthropic a top-level effort change resets the messages cache.
- **Masking** has three modes (`context.masking`):
  - `client`: gitagent's own batched stubs, as in the blueprint. Each stub keeps the exit status, line count and spill file.
  - `server`: Anthropic's context editing (`clear_tool_uses_20250919`) with trigger `mask_batch_tokens`, keep `keep_last` and clear-at-least `mask_batch_tokens / 2`. It is batched like the blueprint's masking and does not count as a history edit.
  - `none`.

  The loader refuses `client` for a Claude profile whose `thinking_binding` is `error`, because client masking edits earlier turns.
- **Prompt size:** taken from the provider's reported input tokens after each response, plus an estimate (bytes / 4) for what was appended since. No tokenizer.
- **Compaction is on by default.** It only runs within `compact_at_tokens` of the context window, or after the provider reports a context overflow (then the turn is retried, at most twice per run). It replaces the history with the first message (task and snapshot) plus the model's structured summary: DONE, PENDING, KEY FILES, LAST FAILURE, RULED OUT, with the task repeated verbatim by us rather than by the model. If the summary request fails, a summary of the last 12 calls is built instead. No thinking blocks are carried across.
- **Cache lifetime:** `cache.ttl: 1h` asks Anthropic for the one-hour cache, which suits long command waits. The default stays `5m`.

## M5: edit tools

- **`apply_patch`** (OpenAI profile) is a compact patch-format edit tool in the style OpenAI models are trained on (Add, Update with optional Move, Delete, `@@` context, `*** End of File`). Lines are matched exactly first, then ignoring trailing whitespace, then both ends, then with typographic punctuation normalized. A patch is checked in full before any file is written, so a failing hunk changes nothing.
- **`str_replace`** (Anthropic, Google and compat profiles) requires one exact match, or `replace_all`. Errors list the line numbers of every match. With no exact match, a whitespace-tolerant fallback compares trimmed lines, applies only to a unique match, and re-indents the replacement to the file. It is counted as `str_replace_fallbacks`, and the tool result says so.
- Successful edits through either tool count as changes for the stop check.

## M7: sandbox and /tests

- **Where the sandbox applies:** to the agent's shell, not to gitagent itself. The shell is started through `gitagent __sandbox --write <dir>… -- <shell>`, which applies Landlock (writes allowed only under the working directory, the output directory, `/tmp`, `/var/tmp`, `/dev` and `~/.cache`; reads unrestricted) and a seccomp deny-list (mount, modules, reboot, keyrings, BPF, clock and host-name changes, …; denied calls fail with EPERM), then `exec`s the shell. Everything the shell starts inherits the restrictions, while gitagent can still write its records.
- **When:** `sandbox: auto` turns it on outside `--bench` on Linux. In `--bench` it is off: the task container is the isolation, and tasks legitimately write to `/etc`, `/usr` and other system paths. macOS has no sandbox; `sandbox: on` there is an error.
- **`/tests` writes are refused in every mode** by a built-in check in the tool dispatcher, not by Landlock. Landlock grants access only and cannot carve one directory out of an allowed tree. The check covers file tools and patches aimed at `/tests`, and shell commands that name `/tests` as a path together with a writing command. Refusals are counted as `protected_writes_blocked`.

## Prompt: no clean-up directive

`prompts/system.md` has no "leave no extra files or side effects" directive. Clean-up and minimal-state directives make agents delete state the result needs (services, generated files, installed packages), so the prompt says the opposite: do not undo what the result depends on.

## Sub-agents (not in the blueprint; off by default)

BLUEPRINT D2 is a single agent, and "multi-agent by default" is excluded. Sub-agents are therefore an opt-in profile switch (`tools.subagents`), to be measured on the dev split like any other feature. The design:

- **Definitions:** a built-in `general` agent plus `agents/<name>.md` files (`name`, `description`, `tools` allowlist, `max_turns`; the body holds the role instructions).
- **The `subagent` tool** runs a nested loop with a fresh history: the `prompt` string is the only handoff. The sub-agent gets its own shell in the parent's current directory, half of the remaining time, and no sub-agents of its own.
- **Results:** only its final summary returns to the parent. Its trajectory is written under `subagents/<n>/` and linked through ATIF `subagent_trajectory_ref`. Its tokens count toward the parent's totals.
- **Execution:** sub-agents run one at a time for now.

## Continuous learning (not in the blueprint)

The blueprint's agent has no memory across tasks. `gitagent learn` adds learning across runs, kept separate from benchmark runs:

- **Process:** offline and reviewed. `gitagent learn --from <harbor job>…` reads each trial's `agent/trajectory.json` and the reward in its `result.json`, and nothing else. It never reads tests, verifier output or solutions. It asks the model (`prompts/learner.md`) for operations on a lesson store, `bench/lessons/lessons.json`: add, edit, upvote, downvote. A new lesson starts at importance 2, and a lesson is deleted when it reaches 0.
- **What is shown to the model:** an accepted and a rejected attempt at the same task go in one request so it can learn from the difference. Other trials go alone.
- **Leakage audit:** every added or edited lesson must pass. The audit rejects:
  - the task name;
  - paths, file names, quoted identifiers or numbers taken from the instruction;
  - code blocks or long code spans;
  - anything over 400 characters or under 5 words.
- **Evidence threshold:** importance rises only on the first vote from each task, so repeated runs of one task are not counted as wider evidence. A lesson reaches `prompts/lessons.md` only after attempts at `--min-tasks` (default 2) different tasks support it. The lessons file is capped at 3000 characters.
- **Integrity:**
  - It learns from the dev split only. It refuses to run without `bench/split.json` unless given `--no-split`, which is meant only for tasks that will go in the dev split.
  - The rendered file is reviewed and committed like any prompt change. Held-out runs use the committed, frozen file. The `lessons` profile switch turns it off.
- **Online mode (everyday use):** `learning: online` in `gitagent.yaml`, off by default and never in `--bench`.
  - After each task the agent completes, the same learner reads that run's trajectory. It is told nobody checked the result. Each run counts as its own task for the threshold.
  - Lessons go to `.gitagent/lessons.json` and `.gitagent/prompts/lessons.md` in the project. In repo mode they are a separate commit on the branch, so they are reviewed with the work.
  - Lessons add up across layers: built-in, then config, project and home `prompts/lessons.md`, rather than one file overriding the others.
- **First pass (2026-10-15):** run on our 5 dev trials (fix-git ×4, kv-store-grpc ×1). It stored 3 lessons, each backed by one task, so none is shown yet. An earlier pass without the per-task rule produced 4 lessons about one task's domain (git merges) out of 5, which is why the threshold exists.

## Split: stratified by category and difficulty, not Terminus 2 pass rate (TASKS M0)

M0 asks for a 45/44 split stratified by category and Terminus 2 pass rate. No Terminus 2 baseline existed when development reached the point of running tasks (no amd64 bench host; the Mac runs tasks under emulation), and running the baseline on all 89 tasks there would have taken many hours. The owner chose (2026-10-15) to create the split from the task.toml `category` and `difficulty` instead, with a fixed seed (`scripts/make-split.py`, seed 20261015), forcing `fix-git` and `kv-store-grpc` into dev because their trajectories had been read. The split is committed and never changes.

## `.claude/` project files and the library API (not in the blueprint)

gitagent stays a from-scratch Rust binary that calls the model APIs itself (BLUEPRINT D1). On top of that it reads the `.claude/` project layout many teams already have, and offers a library API, delivered in steps:

1. **File formats (done).** `.claude/agents/*.md`, `.claude/skills/` and the `hooks` object of `.claude/settings.json` (project and home) are read after gitagent's own layers. Agent files accept `tools` / `disallowedTools` (a comma string or YAML list), `maxTurns`, `model` and `effort`; the tool names used there are translated to gitagent's (`Bash` → `exec`, `Read` → `read_file` + `image_read`, `Edit` → `str_replace` + `apply_patch`, …) and names with no equivalent (`Grep`, `Glob`, `WebFetch`) are dropped. Settings hooks keep the protocol their scripts expect: `PreToolUse` / `PostToolUse` groups with `matcher` (`*`, `Edit|Write`, or a regex), `command` handlers with `timeout` and `${CLAUDE_PROJECT_DIR}`, stdin with `hook_event_name`, `tool_name`, `tool_input` (translated argument names), `tool_use_id` and `tool_response`, exit 2 blocks with stderr as the reason, other failures are non-blocking, and `hookSpecificOutput.permissionDecision` / `updatedInput` / `updatedToolOutput`, `decision: "block"` and `continue: false` are honoured. gitagent's own script hooks keep their stricter rule (any failure blocks) and run first. `--bench` ignores all of these layers, as it does every project file.
2. **Library API (done):** the `gitagent-sdk` crate. `query(prompt, AgentOptions)` runs the same loop and streams `Message::System` (`init`, `compact_boundary`), `Assistant`, `User` and `Result` (`session_id`, `parent_tool_use_id`, `num_turns`, `duration_ms`, `total_cost_usd`, `usage`, `modelUsage`, `permission_denials`); tool uses and results appear under the `.claude/` tool and argument names. `AgentOptions` covers `model`, `systemPrompt` / append, `cwd`, `allowedTools`, `disallowedTools`, `maxTurns`, `agents` (`AgentDefinition`), `hooks` (callbacks with `HookInput` / `HookOutput`) and `settingSources` (default none). Not offered: `permissionMode` (gitagent has no interactive permissions; the stream reports `bypassPermissions`), `resume`, `env`, `mcpServers`, `maxBudgetUsd`. Notes: `tool_result` content in the stream is cut at 2000 characters (the trajectory holds the full text); `duration_api_ms` counts time from each turn's start to the model's reply; `total_cost_usd` is 0 unless `prices` are given. The CLI gained `--output-format stream-json`, `--max-turns`, `--allowed-tools`, `--disallowed-tools` and `--append-system-prompt`. Provider construction moved from the CLI into `gitagent_provider::for_spec`.
3. **`.claude/` tool names offered to the model (built, off, unmeasured):** `tool_naming: sdk` in a profile renames the tool definitions the model sees (`Bash` / `command` / `timeout`, `Read` / `file_path` (images included; `image_read` folds into it), `Write`, `Edit` / `old_string` / `new_string`, `Agent`, `Skill`, `TodoWrite`); tools without such a name (`write_stdin`, `apply_patch`, `task_complete`) keep theirs. The loop maps each call back to gitagent's tool before hooks, guards and dispatch, counting `sdk_named_calls`; the trajectory records the name the model used. The default stays `gitagent` until a dev-split comparison says otherwise.
4. **Sub-agent features (built, off until measured):** a sub-agent's `model` (an alias such as `sonnet` or a model id, same provider as the parent) and `effort` are applied, each with a counter; sub-agents called in one reply run at the same time, `tools.max_concurrent_subagents` (default 4) at once, with their hooks asked first; `tools.subagent_depth` (default 1) says how deep they may nest; a sub-agent that hits `maxTurns` returns what it had as a partial result, not an error. Not done: background sub-agents and resuming one, and a dollar budget cap (`maxBudgetUsd`); gitagent's time budget bounds sub-agents instead (each gets half the time left).
