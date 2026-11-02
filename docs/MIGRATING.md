# Migrating to gitagent 3

In gitagent 3 the agent runs in one Rust binary. The npm package `@open-gitagent/gitagent` is still how most people install it: it now brings the binary along, and it still provides the `gitagent` command and the TypeScript SDK. Most agents need no changes. This guide goes through the upgrade step by step and lists the few things that behave differently.

## At a glance

| | |
|---|---|
| Agent folders, hooks, schedules, workflows, plugins | Run unchanged |
| The `gitagent` command and its flags | The same, including `-s`/`--sandbox` through npm |
| `query()` and `tool()` in TypeScript | The same |
| Exit codes | No longer always 1 on error ([step 4](#4-update-scripts-that-check-exit-codes)) |
| Unattended runs | Stop after 30 minutes unless you set a limit ([step 5](#5-set-a-time-limit-if-your-unattended-runs-need-longer-than-30-minutes)) |
| The agent's shell | Sandboxed on your machine by default ([step 6](#6-check-what-the-agent-writes-outside-its-folder)) |
| Voice | Not in gitagent 3; stay on 2.x if you need it |

## 1. Install gitagent 3

If you installed gitagent with npm, update it the same way:

```sh
npm install -g @open-gitagent/gitagent@latest
gitagent --version          # 3.0.0
```

This installs the binary for your platform (Linux x86_64/arm64, macOS arm64/x86_64) as a small extra package, `@open-gitagent/gitagent-<os>-<cpu>`, and a `gitagent` command that runs it. On other platforms, build from source and set `GITAGENT_BIN` to the binary. If you install with `--omit=optional`, the binary package is skipped; set `GITAGENT_BIN`, or use the install script instead.

Without npm, the install script puts the binary in `~/.local/bin` (set `INSTALL_DIR` to put it somewhere else):

```sh
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitagent/main/scripts/install.sh | sh
```

If you have both, whichever `gitagent` comes first on your `PATH` runs. Only the npm command runs the remote sandbox ([step 8](#8-the-remote-sandbox---sandbox)).

## 2. Check your agent folder

```sh
gitagent validate ./my-agent
```

`agent.yaml`, `SOUL.md`, `RULES.md`, `memory/`, `skills/`, `tools/`, `hooks/`, `schedules/`, `workflows/`, `agents/`, `config/` and `.env` are read as they were. 1.x settings such as `tools: [cli, read, write, memory]`, `runtime.timeout` and the 1.x `compliance` shape are still accepted. `validate` reports the newer spelling as a warning, never an error, with the file and line, so you can update at your own pace. Agent folders in the OpenGAP layout load as well.

## 3. Run it once

```sh
gitagent -p "summarise what this agent does" --dir ./my-agent
```

The command line you used before works as it did:

- **Flags:** `-p`/`--prompt` or the task as plain words, `-d`/`--dir`, `-m`/`--model`, `-e`/`--env`, `-r`/`--repo`, `--pat` and `--session`. `-s`/`--sandbox`, `--sandbox-repo` and `--sandbox-token` run the remote sandbox through the npm command ([step 8](#8-the-remote-sandbox---sandbox)).
- **Subcommands:** `gitagent plugin install|list|enable|disable|remove|init`, with 1.x's `ls`, `rm` and `create`.
- **The console:** `/memory`, `/skills`, `/plugins`, `/exit` and `/quit`. `/tasks` and `/learned` belonged to 1.x's skill learner; they now say what replaces it (lessons, [step 12](#12-what-changed-or-went-away)). `/help` lists the new ones: `/agents`, `/tools`, `/workflows`, `/schedules`, `/new` and `/clear`.

## 4. Update scripts that check exit codes

| Exit code | gitagent 3 |
|---|---|
| 0 | The run ended, including when it ran out of turns or time |
| 2 | Usage or setup error, also a flag for a feature that is gone (`--voice`) |
| 3 | The model provider failed |
| 130 | The run was stopped |

1.x exited with 1 for every error. A script that only checks for a nonzero code works as before; one that checks for exactly 1 needs updating.

## 5. Set a time limit if your unattended runs need longer than 30 minutes

Runs nobody watches (`-p`, schedules, the SDKs) now stop after 30 minutes by default. Raise the limit with `timeout_s` in `gitagent.yaml`, `--timeout-s` on the command line, or `timeoutMs` over IPC. The console has no limit unless `timeout_s` is set.

## 6. Check what the agent writes outside its folder

Outside benchmark runs, the agent's shell now runs in a sandbox on your machine. It may write only to the working folder, the output folder, the temp folders and `~/.cache` (and `~/Library/Caches` on macOS). Reading is not restricted. If your agent legitimately writes elsewhere, or a container already isolates it, set:

```yaml
# gitagent.yaml
sandbox: off        # auto (default) | on | off
```

This is separate from the remote sandbox in [step 8](#8-the-remote-sandbox---sandbox), which runs the agent's commands in a cloud VM.

## 7. SDK code

- **TypeScript.** `query()`, `tool()`, hooks, `steer()`, `abort()`, the message types and the `repo` option are unchanged, and tools keep their 1.x names (`cli`, `read`, …). The binary comes with the npm package; to use another one, set `GITAGENT_BIN` or pass `binary` to `query()`. `import ... from "@open-gitagent/gitagent/cli"` still runs the command. A `tool()` handler still gets the arguments and an `AbortSignal`, and may return a string, 1.x's `{text, details}` or `{content, isError}`.
- **The `sandbox` option** of `query()` and `createSandboxContext()` work as in 2.x ([step 8](#8-the-remote-sandbox---sandbox)).
- **Your own tools in place of the built-in ones.** With `replaceBuiltinTools: true`, the tools you pass to `query()` are the agent's only tools, and one of yours may use a built-in name such as `cli` or `memory`.
- **Other 2.x exports.** 2.x also exported the harness's internals, such as `loadAgent`, `discoverSkills`, `CostTracker`, the schedule, history, compaction and telemetry helpers, and the voice types. The binary does this work now. These names still exist, so imports keep working, but they are marked deprecated, and calling one throws a `GitagentError` that names the replacement (for costs, `q.costs()`; for schedules, `gitagent schedule`; for sessions, `sessionId` with `resume: true`).
- **Python.** The new client in `clients/python` has the same shape: `query()`, `tool()`, hooks and an approver, plus `send()`, `steer()`, `abort()` and `end()`. It needs the binary installed, and has no remote sandbox option.
- **Other languages.** Start `gitagent --ipc` and exchange JSON lines. The README's [SDKs](../README.md#sdks-any-language-over-ipc) section has the full protocol and a complete client in bash.

## 8. The remote sandbox (`--sandbox`)

The remote sandbox works as in 2.x, through the npm command. The agent runs on your machine, so your API keys stay there; its commands and file edits run in an E2B VM with a clone of the repository, which is committed when the VM stops. It needs the `gitmachine` package, as before:

```sh
npm install -g @open-gitagent/gitagent gitmachine
gitagent --sandbox -p "fix the failing test"                  # clones the folder's origin
gitagent -s --sandbox-repo https://github.com/org/app.git     # one task per line; exit or Ctrl-D ends
```

- **Flags:** `--sandbox-repo` (default: the folder's `origin`), `--sandbox-token` (default: `GITHUB_TOKEN` or `GIT_TOKEN`), `--session` to continue a session branch, and `-d`, `-m`, `-e` and `-p`. `--repo` cannot be combined with it, as in 2.x.
- **In the SDK:** `query({ ..., sandbox: true })`, or `sandbox: { repository, token, session, template, timeout, autoCommit, envs }`, starts a VM for the session and stops it, committing the work, before the iteration ends. A `SandboxContext` you started yourself with `createSandboxContext()` is used as it is and left running for you to stop.
- **The tools:** in the sandbox the agent uses 2.x's tools (`cli`, `read`, `write`, `edit`, `memory`) instead of gitagent 3's shell, so background sessions and `write_stdin` are not available there. `memory` saves are committed in the VM, and lines over the working layer's `max_lines` move to `memory/archive/`, as before.
- **Errors:** without `gitmachine` or a repository, the command says what is missing and exits with 2.
- **Not here:** the binary from the install script, the Python client and `--ipc` do not run the remote sandbox. The binary says so and points to the npm command.

## 9. Hooks

Hook scripts get the same JSON and answer the same way. Output that is not an answer, a crash or a timeout allows the action with a warning, as before. New: a hook marked `compliance: true` or `fail_open: false` blocks the action in those cases instead. Hooks get 10 seconds unless `hooks.yaml` sets `timeout`.

## 10. Model settings

`model.constraints` (`temperature`, `max_tokens`, `top_p`, `top_k`, `stop_sequences`) are sent to the providers whose API accepts them. OpenAI's API has no `top_k` or stop sequences, so those two are ignored there with a warning. Current Claude models reject `temperature`, `top_p` and `top_k`, so remove them from Claude agents; `gitagent validate` warns about them.

## 11. Telemetry

Set `OTEL_EXPORTER_OTLP_ENDPOINT` to export traces and metrics (OTLP over HTTP/JSON); `OTEL_EXPORTER_OTLP_HEADERS` and `OTEL_SERVICE_NAME` are honoured. 1.x's `GITAGENT_OTEL_ENABLED` is no longer read.

## 12. What changed or went away

| Before | In gitagent 3 |
|---|---|
| `task_tracker`, a searchable task list | `todo`, a plain checklist the agent rewrites whole; the name `task_tracker` still maps to it in tool lists and hooks |
| Online skill learning, which adjusted a skill's confidence after each use, with `/tasks` and `/learned` | Lessons learned from finished runs (`learning: online` or `gitagent learn`), added to the system prompt; skills are not changed |
| Every declared tool ran with `sh` unless `runtime` was set | The interpreter is picked by extension (`.py` with `python3`, `.js` with `node`, `.rb` with `ruby`, …); `.sh` still runs under `sh` unless its `#!` line names another shell |
| A plugin's `entry` JavaScript | Plugins contribute tools, skills, hooks and prompt text through files; plugin settings reach scripts as environment variables (`GITAGENT_PLUGIN_CONFIG` holds them all as JSON) |
| The 1.x telemetry adapter (`GITAGENT_OTEL_ENABLED`) | OTLP export ([step 11](#11-telemetry)) |
| `--voice`/`-v` and `@open-gitagent/voice` | Not available; the command prints a one-line note and exits with 2. Stay on 2.x if you need voice |

## 13. Optional: new things worth turning on

- **Parallel work:** `runtime: {parallel_tasks: true}` in `agent.yaml` gives the lead agent the `plan` tool.
- **Lessons:** `learning: online` learns from each finished task into `.gitagent/prompts/lessons.md`; review that file like code.
- **Records:** every run writes `trajectory.json`, `counters.json` and the output of every command to `--out-dir`. `--events jsonl` streams each step as it happens.

## Going back to 2.x

```sh
npm install -g @open-gitagent/gitagent@2
```

Agent folders written for 2.x still work there. If you changed settings to the newer spellings that `gitagent validate` suggests, 2.x may not read them, so keep the old spellings until you no longer need 2.x.

Something not covered here? Open an issue with the output of `gitagent validate` and the command you ran.
