# How gitagent works, in detail

This is the mechanical account: what each part of the harness does, with the numbers, formats and algorithms as they are in the code. The README tells the same story at a higher level. File references are to the crates under `crates/`.

## 1. The agent is a folder, and the prompt is assembled from it

`gitagent_loader` reads the agent directory and the configuration layers (`~/.gitagent/`, the project's `gitagent.yaml`, the agent's `agent.yaml`, its `config/<env>.yaml`) into one `AgentSpec`. The system prompt is built in a fixed order:

1. identity (`SOUL.md`, with the name and version from `agent.yaml`),
2. the built-in working instructions (`prompts/system.md`: read the requirements, explore, small steps, verify by exercising the result, finish with `task_complete`),
3. learned lessons, when a profile enables them,
4. skills (`skills/*/SKILL.md`, name and description, instructions inlined),
5. sub-agents, workflows, knowledge documents marked `always_load`, examples, plugin prompts, MCP servers,
6. compliance constraints derived from `agent.yaml`,
7. memory (`memory/MEMORY.md` and the other layers),
8. `hooks/bootstrap.md`.

The same assembly runs for the console, the SDKs and a single task run with `-p`; only the files differ.

## 2. The loop

`gitagent_core::run` is one function. Each turn:

1. The guards run: the time budget may append a reminder, the context manager may mask or compact (section 6).
2. The model is called with the system prompt, the history and the tool specs. Anthropic requests carry cache markers on the system prompt and the last two messages (`cache.breakpoints: [system, last_two_messages]`).
3. Tool calls are dispatched in the order the model gave them, except sub-agent calls, which are collected first and run concurrently (section 9).
4. Each result is shaped (section 4), checked by the loop detector (section 5), recorded, and appended as a tool result. Reminders go after the results of the turn as a plain text block.
5. The turn ends when every call has a result; the run ends when the model calls `task_complete` and the stop check accepts it (section 7), or on the time limit, the turn limit, a kill switch or a provider failure.

Nothing earlier in the history is ever edited, except by the two context operations in section 6, and those are designed around the cache.

## 3. The shell

Every command runs in one persistent shell on a pseudo-terminal (`gitagent_terminal`). The shell is started interactively with line editing, echo and prompts off, and with an environment that turns off pagers, colour and interactive prompts (`TERM=dumb`, `NO_COLOR=1`, `PAGER=cat`, `GIT_TERMINAL_PROMPT=0`, `DEBIAN_FRONTEND=noninteractive`, `PYTHONUNBUFFERED=1`, among others).

A command is not typed into the shell. It is written to a script file, and the shell is given one short line:

```
. 'script'; printf '\n__GA_END_{seq}__ %s %s\n' "$?" "$PWD"
```

The end marker carries the exit status and the working directory, and because the command itself is sourced from a file, quotes, heredocs and commands that read stdin cannot swallow the marker. Working directory, environment and background jobs carry from one turn to the next, like a person's terminal.

A command that has not finished after `exec_default_yield_ms` (10 seconds by default, `runtime.timeout` in agent.yaml) is left running in the background: the model gets the output so far and a session id, and can wait for more, send input or interrupt it with `write_stdin`. Up to `MAX_SESSIONS` shells exist at once. With `--linger` the shells stay open after the run, so a server the agent started keeps serving whoever needs it next.

## 4. The output shaper

Every tool result passes through `gitagent_shaper::shape` before the model sees it. The full raw output is written first to a spill file, `{out-dir}/out/{n}.log`, so nothing is lost. Then:

1. **Binary detection.** Output that is not text is replaced by a one-line note with the byte count and the spill path.
2. **Normalisation for comparison.** Each run of digits in a line becomes one `#`, so `step 9/10` and `step 10/10` compare equal, and timestamps and counters do not defeat the next step.
3. **Collapsing.** A run of `collapse_min_run` or more similar lines (3 by default) is replaced by its first line, a note saying how many were hidden, and its last line. The last line of a progress or build run is usually the one that matters.
4. **The budget.** The result is capped at `cap_tokens` (8,000 by default). Tokens are estimated at four bytes each; no tokenizer is linked, to keep the binary small. `head_ratio` (0.3) says how much of the budget comes from the start; the rest comes from the end. When no whole line fits, as with a minified file or one enormous line, characters are cut from the middle instead.
5. **The status line.** Exit status, line count, and, only when something was hidden, the spill path, so a short output stays short.

The counters record `total_lines`, `hidden_lines` and `collapsed_runs` for every result.

## 5. The loop detector

`gitagent_guards::LoopDetector` keeps a window of the last `loop_repeats` calls (3 by default). Each call is reduced to one 64-bit hash of:

- the tool name,
- the arguments as JSON, digit runs normalised,
- the first 20 and the last 20 lines of the output, each digit-normalised.

When every hash in the window is equal, the model's next turn gets the line *"You have repeated the same action with the same result. Change approach."* appended after the results, and the window is cleared, so the warning repeats only if the loop continues. Raising the model's effort for one call after a warning exists as a switch (`loop_effort_bump`) and is off, because on Anthropic a top-level effort change resets the message cache.

## 6. Context: budget, masking, compaction

**Prompt size.** After each response the provider's reported input tokens are taken as the size of the prompt, plus an estimate of four bytes per token for what has been appended since.

**The time budget.** `gitagent_guards::TimeBudget` is given the run's limit: `--timeout-s`, `timeout_s` in `gitagent.yaml`, or `timeoutMs` over IPC. Without one, runs nobody watches get 30 minutes and the console has no limit, so a console turn shows no time left and gets no reminders. Every command result ends with `· N min left`. The first time 70% of the budget has passed (`nudge_at`) a reminder is appended; at 90% (`force_stop_at`) a final call to finish now. After the final call the stop check accepts `task_complete` without evidence, because a rejection could no longer help. A timeout counts as a failure, so a verified partial result beats running out of time.

**Masking** (`gitagent_context::Masker`) hides old tool results. It has three modes:

- `server`: Anthropic's own context editing. The request carries `context_management: {edits: [{type: clear_tool_uses_20250919, trigger: {input_tokens: 50000}, keep: {tool_uses: 8}}]}`. The provider clears old tool results server-side once the prompt grows past the trigger and keeps the last eight. This is the preferred mode: it does not count as a history edit, so the cache and the model's thinking blocks survive.
- `client`: gitagent's own stubs. When a result is produced, a stub is recorded for it with the exit status, the line count and the spill path. Masking happens only at checkpoints, every `mask_batch_tokens` (50,000) of growth, never in between, because every edit resets the cache from the edited message onward. At a checkpoint all results except the last `keep_last` (8) are replaced by their stubs, images included, and the checkpoint moves to the new size. The loader refuses this mode for a Claude profile whose `thinking_binding` is `error`, since a client edit would invalidate thinking blocks.
- `none`.

**Compaction** replaces the history when the prompt is within `compact_at_tokens` (16,000) of the context window, or after the provider reports a context overflow (the turn is then retried, at most twice per run). The model is asked, without tools, for a summary in exactly five sections: DONE, PENDING, KEY FILES, LAST FAILURE, RULED OUT. The new history is one user message: the original first message verbatim (the task, and the snapshot if any), then `# Progress so far` and the summary. The task is repeated by the harness, not paraphrased by the model, so the goal cannot drift. If the summary request itself fails, a mechanical summary is built instead: the last 12 tool calls with the last line of each result. No thinking blocks are carried across.

## 7. Done means verified: the stop check

`gitagent_guards::StopCheck` watches two things, each stamped with the sequence number of the tool call:

- **Edits.** A file tool call (`write_file`, `str_replace`, `apply_patch`), or a shell command matching one of the edit patterns after redirections to `/dev/null` and between file descriptors are removed: `sed -i`, `perl -i`, `>` and `>>` into a file, `tee`, `mv cp rm ln chmod patch`, `git apply/am/checkout/switch/merge/cherry-pick/rebase/reset/restore/revert/commit/stash pop`, package installs (`pip npm pnpm yarn cargo apt apk yum dnf gem conda … install/add`).
- **Verifications.** A *finished* command matching one of the verify patterns: test runners (`pytest`, `tox`, `npm test`, `cargo test`, `go test`, `make`, `ctest`, `jest`, `mocha`, `vitest`, `rspec`, `phpunit`, Maven and Gradle tests), running a program (`python x.py`, `node x.js`, `./x`, `bash x.sh`, `java`, `ruby`, `perl`, `php`, `Rscript`, `julia`, `lua`), sending a request (`curl`, `wget`, `nc`), `diff`/`cmp`, database clients, and `git clone/push/log/status/diff/show`. A command that both edits and verifies (`sed -i … && pytest`) counts as verified.

When the model calls `task_complete`, the verdict is Accept if the last verification came after the last edit, otherwise Reject: the result tells the model which change was not verified and repeats the original task. At most `max_rejections` (2) rejections happen per run; after the time budget's final call the check accepts. The patterns are deliberately broad on the verify side, so the check only catches completions with no evidence at all. The check never rewrites the agent's work.

## 8. The environment snapshot

One probe runs at startup as its own process with a 15-second limit, never in the agent's shell, so it leaves no shell state. It collects the working directory, the user, `uname -srm`, CPUs, memory from `/proc/meminfo` (not every image has `free`), the first 40 lines of `ls -la`, and the versions of the tools present among `python3 pip3 node npm go rustc cargo gcc g++ java make cmake git docker psql`. The result, at most 4,000 characters, goes into the first user message after the task. On any failure the snapshot is left out and `snapshot_failed` is counted.

## 9. Sub-agents and parallel work

A sub-agent (`subagent` tool, or an `agents/<name>/` directory with its own files) is a full run of the same loop with a fresh context, its own shell in the parent's working directory, the parent's time left, hooks, MCP servers and approver, and `subagent_depth` reduced by one, so nesting is bounded. Sub-agent calls made in one reply are collected before any tool runs and executed in chunks of `max_concurrent_subagents` (4 by default) at the same time; only each one's final summary comes back.

With `runtime.parallel_tasks: true`, the lead agent also gets the `plan` tool (`crates/core/src/plan.rs`): phases of tasks, up to 10 phases and 100 tasks. Every task in a phase runs as a sub-agent, `max_parallel` at a time; from the second phase on, each task's prompt ends with `# Results of the earlier phases` and the results so far; when all phases are done, one more sub-agent runs the plan's `reconcile` instruction with `# All results`, and its answer is the tool's result. Each task's trajectory is kept under `plans/<call>/tasks/<n>/`; `plan` events (phase_start, task_start, task_end, phase_end, reconcile_start, reconcile_end) drive the console's task board.

## 10. The records

Every run writes, under its output directory:

- `trajectory.json` in the ATIF format: the system prompt, the instruction, and one step per turn with the model's message, its reasoning, the tool calls and each observation.
- `counters.json`: every decision the loop made, by name: turns, tool calls per tool, loops caught, stop-check rejections, masked results and tokens saved, compactions, cache reads and writes, hook blocks, approvals, sub-agent and plan counts, guard events.
- `out/{n}.log`: the spill files.
- events as JSON lines when asked (`--events jsonl`), and, for agents with audit logging, `.gitagent/audit*.jsonl`.

Because the record is complete, a failed run can be read step by step, and a change to the harness can be measured run by run against the previous one.

## 11. The sandbox

On Linux, with `sandbox: auto` or `on`, the agent's shell, not gitagent itself, starts through `gitagent __sandbox`, which applies Landlock (writes allowed only under the working directory, the output directory, `/tmp`, `/var/tmp`, `/dev` and `~/.cache`; reads unrestricted) and a seccomp deny-list (mount, kernel modules, reboot, keyrings, BPF, clock and host-name changes; denied calls fail with `EPERM`), then execs the shell. Everything the shell starts inherits the restrictions, while gitagent can still write its records. Where a container already provides the isolation, the sandbox can be turned off.

## 12. Learning between runs

gitagent can turn its own finished runs into short, general lessons that later runs see in their system prompt. Nothing is ever learned during a run.

**Offline,** `gitagent learn --from <runs>` reads finished runs: only gitagent's own trajectory of each and whether it succeeded. Runs of the same task are grouped into units, a passed and a failed attempt side by side when both exist, so the learner can see what made the difference. The learner model (`prompts/learner.md`) gets one unit and the current lessons, and answers with at most three operations:

| Operation | Effect |
|---|---|
| `add` | A new lesson at importance 2, supported by this task. Skipped when it shares at least 80% of its words with an existing lesson |
| `edit` | A lesson rewritten to be more accurate or more general; counts as a vote from this task |
| `upvote` | The attempt shows the lesson helps or is right: `helpful` + 1, a vote from this task |
| `downvote` | The attempt shows the lesson hurt or is wrong: importance − 1, `harmful` + 1; at 0 the lesson is deleted |

A vote from a task raises a lesson's importance only the first time that task votes, so running one task many times cannot make a lesson look widely supported.

**The audit.** Every added or edited lesson is checked before it is stored, and rejected when it is over 400 characters, under five words, contains a code block or a code span over 40 characters, or names the task or repeats a path, file name, identifier or number from its instruction. Rejections are counted.

**What reaches the prompt.** The store keeps every lesson with its evidence. The prompt's `prompts/lessons.md` is rebuilt from it on every learning run: lessons supported by at least two different tasks, highest importance first, until the file would pass 3,000 characters. A lesson seen on one task waits in the store for a second. The same lessons go to every run, so the system prompt stays byte-identical and cached. The file is reviewed like any prompt change, committed, and compiled into the binary; a profile can leave lessons out with `lessons: false`.

**Online,** `learning: online` in `gitagent.yaml` runs the same learner after each finished task in a project, into the project's `.gitagent/prompts/lessons.md`, which later runs load on top of the built-in lessons. A lesson's support is the learner model's judgment that an attempt shows it, not a measured cause; whether lessons help is measured by comparing runs with and without them.

## 13. Memory, hooks and approvals

**Memory** is files in `memory/`, with layers and line limits from `memory/memory.yaml`. The `memory` tool's `save` appends an addition; a save that sends the whole memory back with edits, carrying at least 60% of the existing lines, replaces the file, as gitagent 1.x did; overflow beyond a layer's limit is archived to `memory/archive/`. Each save is a git commit when the agent directory is a repository.

**Hooks** are scripts in `hooks/hooks.yaml` for seven events (session start and end, before and after a tool call, a changed file, before the reply, on error) plus 1.x's `user_prompt_submit`. Each gets JSON on stdin and answers `{"action": "allow" | "block" | "modify", ...}`; a script that prints something else allows, and one that crashes or times out is a warning. A hook marked `compliance: true` or `fail_open: false` fails closed instead: a crash, a timeout or an answer it cannot read blocks the action. The same protocol serves the SDKs' in-process hooks over the IPC stream.

**Approvals** come from `agent.yaml`'s compliance section: human-in-the-loop rules, escalation triggers by tool, kill switch, segregation of duties, spending limits with a monthly ledger in `.gitagent/state.json`. A gated call pauses the run until a person answers, at the terminal, through the SDK's approver, or by a timeout policy.
