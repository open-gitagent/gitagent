# gitagent 3 — Rust Engine Blueprint

`BLUEPRINT.md` says **what** to build and **why**; `TASKS.md` says **in what order** and **how to know each step is done**.

---

## 0. Goal in one paragraph

Build gitagent v2 **from scratch** as a single static **Rust** binary. It keeps the gitagent name and one idea from v1 (all agent behavior lives in version-controlled files) but reuses no v1 code and keeps no v1 compatibility. Its core is an **evidence-gated minimal loop** proven to score well on Terminal-Bench 2.1: one agent, one append-only history, about seven tools, persistent PTY sessions, an environment snapshot, output shaping with spill files, batched observation masking, time and loop guards, and a stop check that refuses "done" until the agent has verified its work. Target: on the same model, beat Terminus 2 by 5+ points on a frozen held-out split, with median time at or below Terminus 2, timeouts under 10%, and ≥80% of input tokens served from the provider cache.


---

## 1. Requirements (every design choice traces to one)

| ID | Requirement |
|---|---|
| R1 | Higher Terminal-Bench 2.1 pass rate than Terminus 2 on the same model (paired, held-out, ≥3 trials) |
| R2 | Works with OpenAI, Anthropic, Google and OpenAI-compatible open-weight endpoints |
| R3 | Median agent time ≤ Terminus 2; timeouts < 10% |
| R4 | ≥80% of input tokens from provider cache on long tasks |
| R5 | Handles REPLs, servers, long builds, Ctrl-C |
| R6 | Never accepts "done" without verification after the last edit |
| R7 | Runs in any Linux container with no dependencies (static musl binary) |
| R8 | All tunable behavior in config (YAML), not code |
| R9 | Every feature emits fired/consumed counters |
| R10 | Trajectories exported in Harbor's ATIF format, written every turn |
| R11 | Integrity: no internet solution lookup, no task-name routing, no writes to `/tests`, no reading verifier code |
| R12 | All behavior lives in version-controlled files (config, prompts, profiles), so every score traces to a commit |

---

## 2. Architecture

```
                         ┌───────────────────────────────┐
  Harbor / CLI / TS SDK  │  gitagent (static Rust binary) │        Model APIs
  ─────────────────────▶ │                               │ ◀────▶ OpenAI, Anthropic,
   instruction + flags   │  loader ── config + profiles  │        Google, open-weight
                         │  snapshot ─ env facts turn 0  │
                         │  controller ── THE LOOP       │
                         │    ├ provider (native tools)  │
                         │    ├ tools: exec/stdin/read/  │
                         │    │        edit/image/todo/  │
                         │    │        complete + yaml   │
                         │    ├ terminal (PTY sessions)  │
                         │    ├ shaper (cap + spill)     │
                         │    ├ guards (time, loops)     │
                         │    ├ context (mask, compact)  │
                         │    ├ stop check (evidence)    │
                         │    └ hooks (optional gates)   │
                         │  record ── ATIF + counters    │
                         │  sandbox ── Landlock/seccomp  │
                         └───────────────────────────────┘
```

### Run lifecycle (numbers are referenced throughout)

1. **Start** — CLI/Harbor/SDK starts the binary with an instruction.
2. **Load** — read `gitagent.yaml` + model profile; build the system prompt from `prompts/system.md` (+ optional `prompts/rules.md` and the skill index). System prompt must be byte-identical across tasks (cache).
3. **Snapshot** — one compound probe (≤15 s, fail-silent) → injected into the **first user message**, never the system prompt.
4. **Model call** — provider adapter, native tools, effort from profile; text fallback + one-line repair request on malformed calls.
5. **Dispatch** — run the tool; shell commands go to PTY sessions with end markers; `pre_tool_use` hooks run first.
6. **Shape** — strip ANSI, keep last progress-bar frame, collapse similar lines, cap (~8K tokens, 30% head / 70% tail), save full output to a spill file, add footer.
7. **Guards** — remaining time in footer; nudge at 70%, force stop check at 90%; 3 identical (command, normalized output) → change-approach message.
8. **Context** — every ~50K tokens of growth, mask tool results older than the last 8 into stubs; structured compaction only near the limit.
9. **Stop check** — `task_complete` accepted only if a verification ran after the last edit; else reject (max 2), re-show instruction.
10. **Record** — append trajectory step + counters to disk **every turn**; final flush on exit.
11. **Grade** (benchmark only) — Harbor runs hidden tests.

Steps 4–9 repeat until 9 accepts or a limit is hit.

---

## 3. Design decisions (short form)

| # | Decision | Why |
|---|---|---|
| D1 | Static Rust binary inside the container | No deps, ms startup, direct PTY, memory-safe, sandbox-ready |
| D2 | Single agent, linear append-only history | Single loops are where the measurable gains are; caching needs it |
| D3 | Native tool calling + text fallback + repair | Reliable calls on every provider, open-weight models included |
| D4 | PTY sessions, end markers, background jobs | No guessed waits; REPLs/servers work |
| D5 | ~7 tools | Small cached prefix; fewer bad choices |
| D6 | Edit tool per model family | apply_patch for GPT, str_replace for Claude |
| D7 | Environment snapshot in first user message | Saves 2–4 turns; cache-safe placement |
| D8 | Shaper + spill files | Biggest token cost; nothing lost |
| D9 | Batched observation masking + cache breakpoints | ~½ cost at same solve rate; keeps cache |
| D10 | Structured compaction near limit only | Keeps requirements and failed approaches |
| D11 | Evidence-based stop check, no answer rewriting | Premature "done" is a top failure; rewriting critics hurt |
| D12 | Time budget + loop breaker | Timeouts are failures |
| D13 | Per-model-family profiles in YAML | Profiles don't transfer across providers |
| D14 | Own thin provider adapters (reqwest+serde) | Byte control for cache hits |
| D15 | Counters + ATIF written every turn | Dead features show up; timeouts keep their record |
| D16 | Landlock + seccomp sandbox, profile-switchable | Safe outside benchmarks |

**Excluded on purpose:** critics that rewrite answers, semantic tool-result caches, cross-task memory during benchmarks, multi-agent by default, best-of-N on Terminal-Bench, automated harness rewriting, masking every turn, third-party multi-provider frameworks.

---

## 4. Product surface (from scratch)

No code from the TypeScript gitagent is carried over. Its file formats, flags and SDK types are accepted for compatibility (see the README's "Coming from the TypeScript gitagent"), but the engine is new. The product grows in this order:

| Capability | Design | Milestone |
|---|---|---|
| Benchmark mode | `gitagent --bench --instruction-file … --model … --profile …`; no network except the model API, no memory, no git commits, `/tests` writes denied | M1 |
| Single prompt | `gitagent -p "…" --dir <folder>`: runs the same loop on a local folder, prints a summary | M6 |
| Interactive | `gitagent --dir <folder>`: REPL on top of the same loop; Ctrl-C interrupts the current turn | M6 |
| Config as files | `gitagent.yaml`, `prompts/`, `profiles/`, optional `skills/` and `hooks/` in the project or `~/.gitagent/` | M6 |
| Repo mode | `gitagent --repo <url> -p "…"`: clone the repo, create a new working branch, run the loop inside it, commit, and optionally push (§5 Repo mode) | M6 |
| Hooks (optional) | `pre_tool_use` / `post_tool_use` scripts get JSON on stdin and return `allow`, `block` or `modify` | M6 |
| Skills (optional) | `skills/<name>/SKILL.md`; index in the system prompt, full text loaded on demand by a `skill` tool | M6 |
| Sandbox, telemetry | Landlock/seccomp on outside `--bench`; OpenTelemetry off unless configured | M7 |
| Later, only if needed | Opening a pull request, MCP client, a TypeScript or Python SDK over `--events jsonl` | after M7 |

---

## 5. Crates and interfaces

Cargo workspace, edition 2021, `#![forbid(unsafe_code)]` everywhere except `sandbox`.

```
gitagent/
├─ crates/
│  ├─ cli/        binary: clap flags, modes (interactive, -p, --bench, --repo)
│  ├─ loader/     gitagent.yaml + prompts + profiles → AgentSpec
│  ├─ provider/   Provider trait; anthropic, openai, google, compat
│  ├─ terminal/   PTY session manager, markers, background sessions
│  ├─ tools/      tool registry, built-ins, yaml tools, edit tools
│  ├─ shaper/     output shaping + spill
│  ├─ context/    history, masking, compaction, token estimates
│  ├─ guards/     time budget, loop detector, stop check
│  ├─ hooks/      optional allow/block/modify script hooks
│  ├─ record/     ATIF writer, counters, audit log, events (JSONL)
│  ├─ repo/       clone, branch, commit, push via the git CLI
│  ├─ sandbox/    landlock + seccomp (linux), no-op elsewhere
│  └─ core/       controller loop wiring everything
├─ profiles/      anthropic.yaml, openai.yaml, google.yaml, compat.yaml
├─ prompts/       system.md, rules.md
├─ harbor/        agent.py (Harbor installed-agent shim)
└─ tests/         fake-provider integration tests, fixtures
```

### Key types (sketch — refine, don't treat as final)

```rust
// provider
pub trait Provider: Send + Sync {
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse>;
    fn family(&self) -> Family;               // Anthropic | OpenAI | Google | Compat
}
pub struct ChatRequest { pub system: String, pub messages: Vec<Message>, pub tools: Vec<ToolSpec>,
                         pub effort: Effort, pub temperature: Option<f32>, pub cache: CachePlan }
pub struct ChatResponse { pub text: String, pub tool_calls: Vec<ToolCall>, pub usage: Usage /* incl. cache_read/write */ }

// terminal
pub struct ExecResult { pub session: SessionId, pub output: Vec<u8>, pub exit: Option<i32>, pub finished: bool }
pub trait Terminal {
    async fn exec(&mut self, cmd: &str, yield_ms: u64, background: bool) -> Result<ExecResult>;
    async fn write_stdin(&mut self, s: SessionId, keys: &str) -> Result<ExecResult>;
    async fn poll(&mut self, s: SessionId, yield_ms: u64) -> Result<ExecResult>;
    async fn kill(&mut self, s: SessionId) -> Result<()>;
}

// shaper
pub struct Shaped { pub text: String, pub spill_path: PathBuf, pub total_lines: usize, pub hidden_lines: usize }
pub fn shape(raw: &[u8], cfg: &ShapeCfg, spill_dir: &Path, seq: u64) -> Shaped;

// guards
pub enum StopVerdict { Accept, Reject { reason: String } }
pub struct StopCheck { last_edit_turn: Option<u32>, last_verify_turn: Option<u32>, rejections: u8 }
```

### Algorithms

**Exec with end marker (terminal)**
```
seq += 1; marker = "__GA_END_{seq}__"
write: "{cmd}\nprintf '\\n{marker} %s\\n' $?\n"
read PTY until line starting with marker OR yield_ms elapsed
if marker seen → finished=true, exit=parsed code, strip marker line
else → finished=false, keep session alive, return partial output + session id
```

**Shaper**
```
write raw bytes to {spill_dir}/{seq}.log            (always, first)
if binary (>10% non-text bytes) → "[binary output, N bytes, saved to {path}]"
text = strip_ansi(raw); for each line keep text after last '\r'
collapse runs ≥3 of lines equal after digits→'#' normalization → first line + "[N similar lines hidden]"
if tokens(text) > cap: keep head 30% + "[… N lines hidden — see {path} …]" + tail 70%
footer: "exit {code} · {total} lines · full log: {path} · {mins} min left"
```

**Batched masking (context)**
```
after each turn: if est_tokens(history) - tokens_at_last_checkpoint ≥ mask_batch_tokens:
    for tool results older than the last keep_last: replace body with stub
        "[output hidden: exit {code}, {lines} lines → {spill_path}]"
    tokens_at_last_checkpoint = est_tokens(history)
never mutate history between checkpoints (cache prefix must stay stable)
if remaining_context < compact_at_tokens: replace history with structured summary:
    REQUIREMENTS (original instruction verbatim) | DONE | PENDING | KEY FILES | LAST FAILURE | RULED OUT
```

**Stop check (guards)**
```
on edit tool call or exec matching edit_patterns (sed -i, >, >>, tee, mv, cp, patch, git apply, …): last_edit_turn = turn
on exec matching verify_patterns (test runners, curl/wget, running a built binary, python -c, node -e,
    git clone/push, psql -c, …) with finished=true: last_verify_turn = turn
on task_complete:
    if last_edit_turn.is_some() && last_verify_turn < last_edit_turn && rejections < max_rejections:
        rejections += 1; return Reject("No verification since your last change. Original task: …")
    return Accept
patterns live in the profile (R8)
```

**Loop detector (guards)**
```
key = hash(tool name + normalized args + normalized first/last 20 output lines)
if last 3 keys identical → append "You have repeated the same action with the same result. Change approach."
    and raise effort one level for the next call if the provider supports it
```

**Repo mode (repo crate)**
```
inputs: --repo <url>, optional --base <branch> (default: remote default branch),
        optional --branch <name> (resume), token from GITAGENT_GIT_TOKEN or GITHUB_TOKEN
clone:  git clone --filter=blob:none <url> <workdir>   (token supplied via GIT_ASKPASS helper,
        never embedded in the URL, never written to .git/config, never logged)
branch: if --branch given and exists on remote → git checkout <branch>   (resume)
        else → git checkout -b gitagent/<slug-of-task>-<6-char-id> origin/<base>
run:    the normal loop with --dir <workdir>; the snapshot includes the branch name and git status
commit: after the stop check accepts (and optionally every N turns, profile setting):
        git add -A && git commit -m "gitagent: <one-line summary>" (author from config; skip if nothing changed)
push:   only with --push: git push -u origin <branch>
rules:  never commit to or push the base branch; never force-push; never rewrite history;
        refuse to start if the working branch equals the base branch
output: final event lists branch, commit SHAs, and push status
```

**Snapshot**
```
one sh -c with `timeout 15`: pwd; ls -la | head -40; uname -a;
for t in python3 pip node npm go rustc cargo gcc g++ java make cmake git docker psql: version or "absent";
free -m; nproc; git status --short | head -20 (if repo); ls of README*, Makefile, package.json, pyproject.toml
→ "[Environment Snapshot]\n…" ; on any failure → omit silently, count `snapshot_failed`
```

---

## 6. Configuration

### `gitagent.yaml`

```yaml
name: gitagent
model: anthropic:<model-id>      # provider:model
fallback_models: []              # optional
max_turns: 200
timeout_s: 1800                  # in --bench, Harbor's limit wins
profile: auto                    # auto = pick by model family; or profiles/<file>.yaml
sandbox: auto                    # auto = off in --bench (container isolates), on otherwise
spill_dir: /tmp/gitagent/out
skills: true
hooks: true
repo:                            # used only with --repo
  branch_prefix: gitagent/
  commit_every_turns: 0          # 0 = commit only when the stop check accepts
  author: "gitagent <gitagent@localhost>"
  push: false                    # --push on the CLI overrides
```

### Profile (`profiles/anthropic.yaml`)

```yaml
family: anthropic
tool_calling: native            # native | native_with_text_fallback | text
edit_tool: str_replace          # str_replace | apply_patch
effort: high
temperature: 1.0                # required by some providers when reasoning is on
exec_default_yield_ms: 10000
shape: { cap_tokens: 8000, head_ratio: 0.3, collapse_min_run: 3 }
context: { mask_batch_tokens: 50000, keep_last: 8, compact_at_tokens: 16000 }
guards: { nudge_at: 0.7, force_stop_at: 0.9, loop_repeats: 3 }
stop: { max_rejections: 2, verify_patterns: [...], edit_patterns: [...] }
cache: { breakpoints: [system, last_two_messages] }
```
`openai.yaml` uses `edit_tool: apply_patch`; `compat.yaml` uses `native_with_text_fallback` and a lower `cap_tokens`. Tune values **only on the dev split**.

---

## 7. Interfaces to the outside

- **CLI**: `--bench`, `--instruction-file`, `-p/--prompt`, `--dir`, `--model provider:model`, `--profile`, `--events jsonl`, `--config`; repo mode: `--repo <url>`, `--base <branch>`, `--branch <name>` (resume), `--push`. Token from `GITAGENT_GIT_TOKEN` or `GITHUB_TOKEN`.
- **Events (`--events jsonl`)**: one JSON object per line on stdout: `turn_start`, `model_text`, `tool_call`, `tool_result`, `stop_check`, `guard`, `turn_end`, `repo` (clone, branch, commit, push), `run_end` (with usage, counters, branch and commit SHAs). Designed for logs and a future SDK.
- **Harbor shim (`harbor/agent.py`)**: installed agent; uploads binary to `/opt/gitagent/gitagent`, runs `gitagent --bench --instruction-file … --model … --profile …`, copies `/tmp/gitagent/trajectory.atif.json`, `out/` and counters back. Never writes to `/tests`.

---

## 8. Observability

Counters (written every turn to `/tmp/gitagent/counters.json`, plus OTel when enabled):
`turns, tool_calls{name}, exec_timeouts, background_sessions, shaped_bytes_in, shaped_bytes_out, spill_reads, collapse_events, masks_applied, compactions, cache_read_tokens, cache_write_tokens, input_tokens, output_tokens, stop_rejections, stop_accepts, loop_triggers, nudges, snapshot_ok, snapshot_failed, malformed_calls, repairs, hook_blocks`.

A feature whose counters stay at zero across a dev run is either broken or useless — investigate before keeping it.

---

## 9. Testing

- **Unit**: shaper (ANSI, `\r`, collapse, cap, binary), marker parsing, stop-check rules, loop hash, masking stubs, profile loading.
- **Integration with a fake provider**: scripted model responses (YAML fixtures) drive the real loop against a real PTY in a temp dir. No network in CI.
- **Prompt stability**: snapshot test that the built system prompt is byte-identical across two different tasks.
- **Benchmark**: Harbor on the dev split, ≥3 trials, paired per-task comparison vs the previous milestone. Held-out only at M5 and release.

---

## 10. Suggested crates

`tokio`, `portable-pty`, `reqwest` (rustls), `serde`, `serde_json`, a maintained YAML crate (check status — `serde_yaml` is archived), `clap`, `strip-ansi-escapes`, `regex`, `tiktoken-rs` (estimates), `sha2`, `uuid`, `tracing` + `tracing-subscriber`, `anyhow` + `thiserror`, `landlock`, `seccompiler`, `opentelemetry` + `opentelemetry-otlp` (M7); `rmcp` only if MCP is added later. Static build targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`.

---

## 11. Open questions for the owner

1. First two model families to optimize for?
2. After M7, which comes first: opening pull requests, MCP, or an SDK?
3. Public release plan: open source from day one, or after the first held-out result?
