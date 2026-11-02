# Migrating to gitagent 3

gitagent 3 replaces the TypeScript packages (1.x and 2.x) with one Rust binary. Most agents need no changes. This guide goes through the upgrade step by step and lists the few things that behave differently.

## 1. Install the binary

```sh
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitagent/main/scripts/install.sh | sh
gitagent --version
```

The binary goes into `~/.local/bin`; set `INSTALL_DIR` to put it somewhere else. The SDKs find it on `PATH`, or through `GITAGENT_BIN`.

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

- **TypeScript.** `query()`, `tool()`, hooks, `steer()`, `abort()`, the message types and the `repo` option are unchanged, and tools keep their 1.x names (`cli`, `read`, …). The package now needs the `gitagent` binary installed. A `tool()` handler still gets the arguments and an `AbortSignal`, and may return a string, 1.x's `{text, details}` or `{content, isError}`.
- **Python.** The new client in `clients/python` has the same shape: `query()`, `tool()`, hooks and an approver, plus `send()`, `steer()`, `abort()` and `end()`.
- **Other languages.** Start `gitagent --ipc` and exchange JSON lines. The README's [SDKs](../README.md#sdks-any-language-over-ipc) section has the full protocol and a complete client in bash.

## 8. Hooks

Hook scripts get the same JSON and answer the same way. Output that is not an answer, a crash or a timeout allows the action with a warning, as before. New: a hook marked `compliance: true` or `fail_open: false` blocks the action in those cases instead. Hooks get 10 seconds unless `hooks.yaml` sets `timeout`.

## 9. Model settings

`model.constraints` (`temperature`, `max_tokens`, `top_p`, `top_k`, `stop_sequences`) are sent to the providers whose API accepts them. OpenAI's API has no `top_k` or stop sequences, so those two are ignored there with a warning. Current Claude models reject `temperature`, `top_p` and `top_k`, so remove them from Claude agents; `gitagent validate` warns about them.

## 10. What changed or went away

| Before | In gitagent 3 |
|---|---|
| `task_tracker`, a searchable task list | `todo`, a plain checklist the agent rewrites whole; the name `task_tracker` still maps to it in tool lists and hooks |
| Online skill learning, which adjusted a skill's confidence after each use | Lessons learned from finished runs (`learning: online` or `gitagent learn`), added to the system prompt; skills are not changed |
| Every declared tool ran with `sh` unless `runtime` was set | The interpreter is picked by extension (`.py` with `python3`, `.js` with `node`, `.rb` with `ruby`, …); `.sh` still runs under `sh` unless its `#!` line names another shell |
| A plugin's `entry` JavaScript | Plugins contribute tools, skills, hooks and prompt text through files; plugin settings reach scripts as environment variables (`GITAGENT_PLUGIN_CONFIG` holds them all as JSON) |
| The 1.x telemetry adapter | OTLP export: set `OTEL_EXPORTER_OTLP_ENDPOINT` |
| `--voice`, the remote `--sandbox` | Not available; the binary prints a one-line note |

## 11. Optional: new things worth turning on

- **Parallel work:** `runtime: {parallel_tasks: true}` in `agent.yaml` gives the lead agent the `plan` tool.
- **Lessons:** `learning: online` learns from each finished task into `.gitagent/prompts/lessons.md`; review that file like code.
- **Records:** every run writes `trajectory.json`, `counters.json` and the output of every command to `--out-dir`. `--events jsonl` streams each step as it happens.

Something not covered here? Open an issue with the output of `gitagent validate` and the command you ran.
