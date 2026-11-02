# Migrating to gitagent 3

gitagent 3 replaces the TypeScript packages (1.x and 2.x) with one Rust binary. Most agents need no changes. This guide goes through the upgrade step by step and lists the few things that behave differently.

## 1. Install the binary

```sh
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitagent/main/scripts/install.sh | sh
gitagent --version
```

The binary goes into `~/.local/bin`; set `INSTALL_DIR` to put it somewhere else.

If you installed gitagent with npm, update it the same way:

```sh
npm install -g @open-gitagent/gitagent@latest
```

This now installs the binary for your platform (Linux x86_64/arm64, macOS arm64/x86_64) and a `gitagent` command that runs it. On other platforms, build from source and set `GITAGENT_BIN`. If you install with `--omit=optional`, the binary package is skipped; set `GITAGENT_BIN`, or use the install script instead.

## 2. Check your agent folder

```sh
gitagent validate ./my-agent
```

`agent.yaml`, `SOUL.md`, `RULES.md`, `memory/`, `skills/`, `tools/`, `hooks/`, `schedules/`, `workflows/`, `agents/`, `config/` and `.env` are read as they were. 1.x settings such as `tools: [cli, read, write, memory]`, `runtime.timeout` and the 1.x `compliance` shape are still accepted. `validate` reports the newer spelling as a warning, never an error, with the file and line, so you can update at your own pace. Agent folders in the OpenGAP layout load as well.

## 3. Run it once

```sh
gitagent -p "summarise what this agent does" --dir ./my-agent
```

The CLI flags you used before work as they did: `-p`, `-d`, `-m`, `-e`, `--repo`, `--pat`, `--session`, and the console's slash commands.

## 4. Update scripts that check exit codes

| Exit code | gitagent 3 |
|---|---|
| 0 | The run ended, including when it ran out of turns or time |
| 2 | Usage or setup error |
| 3 | The model provider failed |
| 130 | The run was stopped |

1.x exited with 1 for every error. A script that only checks for a nonzero code works as before; one that checks for exactly 1 needs updating.

## 5. Set a time limit if your unattended runs need longer than 30 minutes

Runs nobody watches (`-p`, schedules, the SDKs) now stop after 30 minutes by default. Raise the limit with `timeout_s` in `gitagent.yaml`, `--timeout-s` on the command line, or `timeoutMs` over IPC. The console has no limit unless `timeout_s` is set.

## 6. Check what the agent writes outside its folder

Outside benchmark runs, the agent's shell now runs in a sandbox. It may write only to the working folder, the output folder, the temp folders and `~/.cache` (and `~/Library/Caches` on macOS). Reading is not restricted. If your agent legitimately writes elsewhere, or a container already isolates it, set:

```yaml
# gitagent.yaml
sandbox: off        # auto (default) | on | off
```

## 7. SDK code

- **TypeScript.** `query()`, `tool()`, hooks, `steer()`, `abort()`, the message types and the `repo` option are unchanged, and tools keep their 1.x names (`cli`, `read`, …). The binary comes with the npm package. `import ... from "@open-gitagent/gitagent/cli"` still runs the command. A `tool()` handler still gets the arguments and an `AbortSignal`, and may return a string, 1.x's `{text, details}` or `{content, isError}`.
- **Other 2.x exports.** 2.x also exported the harness's internals, such as `loadAgent`, `discoverSkills`, `CostTracker`, the schedule, history, compaction and telemetry helpers, and the voice types. The binary does this work now. These names still exist, so imports keep working, but they are marked deprecated, and calling one throws a `GitagentError` that names the replacement (for costs, `q.costs()`; for schedules, `gitagent schedule`).
- **Python.** The new client in `clients/python` has the same shape: `query()`, `tool()`, hooks and an approver, plus `send()`, `steer()`, `abort()` and `end()`.
- **Other languages.** Start `gitagent --ipc` and exchange JSON lines. The README's [SDKs](../README.md#sdks-any-language-over-ipc) section has the full protocol and a complete client in bash.

## 8. The remote sandbox (`--sandbox`)

The remote sandbox works as in 2.x, through the npm command. The agent runs on your machine, so your API keys stay there; its commands and file edits run in an E2B VM with a clone of the repository, which is committed when the VM stops.

```sh
npm install -g @open-gitagent/gitagent gitmachine
gitagent --sandbox -p "fix the failing test"                  # the folder's origin
gitagent -s --sandbox-repo https://github.com/org/app.git     # one task per line
```

`--sandbox-token` (default: `GITHUB_TOKEN` or `GIT_TOKEN`), `--session`, `-d`, `-m`, `-e` and `-p` work with it as before. In the SDK, `query({ ..., sandbox: true })` or `sandbox: { repository, token, template, timeout, autoCommit, envs }` does the same and stops the VM before the iteration ends. In the sandbox the agent uses 2.x's tools (`cli`, `read`, `write`, `edit`, `memory`) instead of gitagent 3's shell, so background sessions and `write_stdin` are not available there. The binary installed with the install script does not run the remote sandbox; it says so and points to the npm command.

## 9. Hooks

Hook scripts get the same JSON and answer the same way. Output that is not an answer, a crash or a timeout allows the action with a warning, as before. New: a hook marked `compliance: true` or `fail_open: false` blocks the action in those cases instead. Hooks get 10 seconds unless `hooks.yaml` sets `timeout`.

## 10. Model settings

`model.constraints` (`temperature`, `max_tokens`, `top_p`, `top_k`, `stop_sequences`) are sent to the providers whose API accepts them. OpenAI's API has no `top_k` or stop sequences, so those two are ignored there with a warning. Current Claude models reject `temperature`, `top_p` and `top_k`, so remove them from Claude agents; `gitagent validate` warns about them.

## 11. What changed or went away

| Before | In gitagent 3 |
|---|---|
| `task_tracker`, a searchable task list | `todo`, a plain checklist the agent rewrites whole; the name `task_tracker` still maps to it in tool lists and hooks |
| Online skill learning, which adjusted a skill's confidence after each use | Lessons learned from finished runs (`learning: online` or `gitagent learn`), added to the system prompt; skills are not changed |
| Every declared tool ran with `sh` unless `runtime` was set | The interpreter is picked by extension (`.py` with `python3`, `.js` with `node`, `.rb` with `ruby`, …); `.sh` still runs under `sh` unless its `#!` line names another shell |
| A plugin's `entry` JavaScript | Plugins contribute tools, skills, hooks and prompt text through files; plugin settings reach scripts as environment variables (`GITAGENT_PLUGIN_CONFIG` holds them all as JSON) |
| The 1.x telemetry adapter (`GITAGENT_OTEL_ENABLED`) | OTLP export: set `OTEL_EXPORTER_OTLP_ENDPOINT`; `GITAGENT_OTEL_ENABLED` is no longer read |
| `--voice`/`-v` and `@open-gitagent/voice` | Not available; the binary prints a one-line note and exits with 2. Keep 2.x if you need voice |

## 12. Optional: new things worth turning on

- **Parallel work:** `runtime: {parallel_tasks: true}` in `agent.yaml` gives the lead agent the `plan` tool.
- **Lessons:** `learning: online` learns from each finished task into `.gitagent/prompts/lessons.md`; review that file like code.
- **Records:** every run writes `trajectory.json`, `counters.json` and the output of every command to `--out-dir`. `--events jsonl` streams each step as it happens.

Something not covered here? Open an issue with the output of `gitagent validate` and the command you ran.
