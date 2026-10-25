# gitagent 3 — Tasks and Milestones

Work top to bottom. Do not start a milestone until the previous one's **Done when** checks pass. Section numbers refer to `BLUEPRINT.md`.

Scores are always: same model, Terminal-Bench 2.1 dev split, ≥3 trials, compared task by task with the previous milestone. Record every run in `bench/RESULTS.md` (date, commit, profile, model, pass rate, timeouts %, median minutes, input/output/cache tokens).

---

## M0 — Measurement setup (no Rust yet)

- [x] Install Docker, `uv`, Harbor; set API keys for two model families.
- [x] Smoke test: `harbor run -d terminal-bench/terminal-bench-2-1 -a oracle -l 5` passes.
- [ ] Baselines on the target model: Terminus 2 (`-k 3`).
- [ ] Create `bench/split.json`: 45 dev / 44 held-out task ids, stratified by category and Terminus 2 pass rate. Commit it. Never change it afterwards.
- [ ] Create `bench/RESULTS.md` with the baseline rows.

**Done when:** baseline scores for the target model are recorded and the split is committed.

---

## M1 — Skeleton: one task end to end

- [x] Cargo workspace with crates from §5 (empty ones may be stubs); CI runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.
- [x] `loader`: parse `gitagent.yaml` + `prompts/` into `AgentSpec`; load profile (§6). Unit tests with fixture configs.
- [x] `provider`: `Provider` trait + **one** adapter (target family) with native tool calling and usage incl. cache tokens.
- [x] `terminal`: one PTY session, `exec` with end marker (§5 algorithm), exit code parsing.
- [x] `core`: loop with tools `exec`, `read_file`, `task_complete`; `max_turns` limit.
- [x] `record`: ATIF trajectory appended **every turn** (follow Harbor's ATIF schema from its repo — do not invent fields); counters file.
- [x] `cli`: `--bench`, `--instruction-file`, `--model`, `--profile`, `--prompt`.
- [x] Static musl build for amd64 and arm64.
- [x] `harbor/agent.py` installed-agent shim; never touches `/tests`.
- [x] Fake-provider integration test: scripted responses run `echo hi`, then `task_complete`.

**Done when:** one easy Terminal-Bench task passes through Harbor end to end, and CI is green.

---

## M2 — Parity with Terminus 2

- [x] `terminal`: multiple sessions, background mode, `write_stdin` (incl. Ctrl-C `\x03`), `poll`, `kill`; yield timeout returns partial output + session id.
- [x] `provider`: second family adapter; text fallback parser; one-line repair message for malformed args; count `malformed_calls`, `repairs`.
- [x] `shaper`: full algorithm (§5) with spill files and footer; unit tests for ANSI, `\r`, collapse, cap, binary.
- [x] `tools`: `write_file` for new files; `image_read` (base64 to provider); optional `todo`.
- [x] System prompt: a short, fixed instruction sheet for our tools; byte-stable across tasks.

**Done when:** dev score within 3 points of Terminus 2 on the same model. If far below, read failed trajectories before adding features.

---

## M3 — Snapshot and stop check

- [x] `snapshot`: probe per §5, injected into the first user message; counters `snapshot_ok/failed`.
- [x] `guards::StopCheck`: edit/verify pattern tracking; reject rules; max rejections from profile; rejection message re-shows the original instruction.
- [x] Unit tests: edit-then-complete is rejected; edit-test-complete is accepted; third attempt accepted after 2 rejections.

**Done when:** dev score above M2 (paired), `stop_rejections` > 0 on some tasks, and fewer probing commands in first 3 turns (spot-check 10 trajectories).

---

## M4 — Time, loops, context and cache

- [x] Time budget: footer with minutes left; nudge at `nudge_at`; force stop check at `force_stop_at`. Read Harbor's task timeout from the environment/flags.
- [x] Loop detector per §5; effort bump where supported.
- [x] `context`: batched masking and structured compaction per §5; token estimates.
- [x] Cache: Anthropic breakpoints on system + last 1–2 messages; never mutate history between checkpoints; counters for cache read/write.

**Done when:** timeouts < 10% on dev, cache read share ≥ 80% of input tokens on runs > 20 turns, and no score loss vs M3.

---

## M5 — Models and edits

- [x] `tools::apply_patch`: patch-format edit tool; Add/Update/Delete file, move; context matching.
- [x] `tools::str_replace`: exact match; clear errors for 0 or >1 matches; whitespace-tolerant fallback, counted separately.
- [x] Profiles for anthropic, openai, google, compat; `profile: auto` picks by model family.
- [x] Third provider adapter + OpenAI-compatible endpoint (test with one open-weight model).
- [ ] Tune profiles on dev only; one change at a time.
- [ ] **First held-out run**: 5 trials, target model, vs Terminus 2.

**Done when:** each family is at or above its M4 dev score, and the held-out run beats Terminus 2 on the same model.

---

## M6 — Product CLI and config

- [x] `-p` single-prompt mode and interactive REPL on a local folder, both using the same loop as `--bench`.
- [x] Config lookup: project `gitagent.yaml`, then `~/.gitagent/`; `prompts/` and `profiles/` resolution.
- [x] Optional hooks: `pre_tool_use` / `post_tool_use` scripts, JSON in, `allow`/`block`/`modify` out; counters `hook_blocks`, `hook_modifies`.
- [x] Optional skills: index in system prompt, `skill` tool loads `SKILL.md` on demand.
- [x] `--events jsonl` event stream (BLUEPRINT §7).
- [x] Repo mode (BLUEPRINT §5): `--repo` clones the repo, creates `gitagent/<slug>-<id>` from the base branch (or resumes `--branch`), runs the loop in it, commits when the stop check accepts, pushes only with `--push`.
- [x] Token handling: read from `GITAGENT_GIT_TOKEN` or `GITHUB_TOKEN`, pass via `GIT_ASKPASS`; test that the token never appears in `.git/config`, logs, events or trajectories.
- [x] Safety tests: refuses to run on the base branch; never force-pushes; no commit when nothing changed; resume continues on the same branch.
- [x] Repo mode is disabled in `--bench`.

**Done when:** a user can run `gitagent -p "fix the failing test" --dir ./project` and `gitagent --repo <url> -p "fix the failing test" --push` on a real project (the second leaves a new branch with the fix pushed), and the benchmark score is unchanged (these features are off in `--bench`).

---

## M7 — Security, telemetry, release

- [x] `sandbox`: Landlock (write only to workspace, spill dir, tmp), seccomp deny-list; on by default outside `--bench`; `/tests` writes denied always.
- [x] OpenTelemetry spans and metrics (turns, tools, tokens, cost), off unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
- [x] Integrity audit of profiles and prompts: nothing task-specific, no task names, no verifier references.
- [ ] Final held-out run (5 trials) on two model families; publish results with public trajectories.
- [x] README, install script, release binaries for Linux amd64/arm64 and macOS.

**Done when:** the release checklist passes and held-out results are published.

---

## Later (only when needed)

- Open a pull request from the working branch (GitHub API).
- MCP client (`rmcp`).
- SDK (TypeScript or Python) over `--events jsonl`.
- Optional fresh-context auditor, only if it wins on dev under default timeouts.
