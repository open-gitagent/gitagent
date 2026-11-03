# gitagent 3.0.0

gitagent 3 is a rewrite of gitagent in Rust. It ships as one static binary with no runtime to install, through npm as before or with a one-line install script, and it runs the agents, hooks, schedules and SDK code written for the TypeScript gitagent (1.x and 2.x) unchanged.

## Upgrading

```sh
npm install -g @open-gitagent/gitagent@latest     # if you installed with npm
gitagent --version                                 # 3.0.0
gitagent validate ./my-agent                       # warns about old spellings, never fails
```

Most agents need no changes. [MIGRATING.md](./MIGRATING.md) goes through the upgrade step by step, and `npm install -g @open-gitagent/gitagent@2` goes back.

## Highlights

- **One binary.** Linux (x86_64, arm64) and macOS (arm64, x86_64). Install with one line; nothing else is needed on the machine.
- **Terminal-Bench 2.1: 93.03%** with Claude Opus 5.5, across all 89 tasks with five runs each (414 of 445 trials), with the code frozen at the published commit. The details and the job logs are in the README under [Results](../README.md#results).
- **One loop everywhere.** The console, the SDKs, schedules and benchmark runs all run the same agent loop, so a benchmark result describes the product.
- **A console in your terminal**, with slash commands, line editing and a live task board.
- **Parallel work.** Sub-agents run concurrently. The `plan` tool splits a large job into phases of tasks, runs them in parallel and reconciles the results. A shared throttle adapts to the provider's rate limits.
- **The remote sandbox stays.** `gitagent --sandbox` (with `gitmachine`) runs the agent's commands and edits in an E2B VM as before, through the npm command; `query({ sandbox })` does the same in the SDK.
- **On npm.** `npm install -g @open-gitagent/gitagent` installs the binary for your platform and the `gitagent` command, so npm users update the way they always have. The same package is the TypeScript SDK.
- **SDKs for any language.** The Python and TypeScript clients drive the binary over a JSON-lines protocol (`gitagent --ipc`), and any language that can start a process can do the same. The Rust crate `gitagent-sdk` runs the loop in your own process.
- **Providers.** Anthropic, OpenAI, Google and any OpenAI-compatible endpoint, with a per-family profile (tools, effort, output shaping, caching) and fallbacks.
- **Finishing means verified.** A run that ends with `task_complete` is accepted only when the agent's last change has been exercised, by a test, a run of the program or a request.
- **Context that stays cached.** The history is append-only. Old tool results are cleared in large, rare batches (by the provider for Claude models), and compaction near the context limit keeps the original task word for word.
- **A sandboxed shell.** Outside benchmark runs, the agent's shell may write only to the working folder, the output folder, the temp folders and the cache folder: Landlock plus seccomp on Linux, `sandbox-exec` on macOS.
- **Learning from finished runs.** `gitagent learn --from <runs>`, or `learning: online` in `gitagent.yaml`, turns finished runs into short lessons. A lesson is used only once two different tasks support it, and the lesson file is reviewed like code.
- **Complete records.** Every run writes its trajectory (ATIF), a counter for every decision the harness made, and the full output of every command. `--events jsonl` streams each step, and OTLP export sends traces and metrics.
- **The OpenGAP format.** Agent folders in the OpenGAP layout load too, and `gitagent validate` checks a folder against the format, giving the file and line of every problem.

## Breaking changes

- **The agent runs in the binary.** The npm package brings it along for Linux and macOS. The Python package and other clients need it installed (with the install script, or `GITAGENT_BIN`).
- **2.x SDK internals.** Exports such as `loadAgent`, `discoverSkills`, `CostTracker` and the schedule, history, compaction and telemetry helpers remain importable but throw when called; see [MIGRATING.md](./MIGRATING.md#7-sdk-code).
- **Voice.** `@open-gitagent/voice` works with 2.x only.
- **Exit codes.** 0 when a run ends (including when it runs out of turns or time), 2 for usage and setup errors, 3 when the model provider fails, 130 when the run is stopped. Before, every error exited with 1.
- **Not carried over:**
  - `--voice`;
  - a plugin's `entry` JavaScript (plugins now contribute tools, skills, hooks and prompt text through files);
  - the 1.x telemetry adapter (OTLP export replaces it);
  - online skill learning (lessons from finished runs replace it).
- **The sandbox is on by default** outside benchmark runs. An agent that writes outside the allowed folders needs `sandbox: off` in `gitagent.yaml`.
- **Unattended runs have a time limit.** `-p` runs, schedules and the SDKs stop after 30 minutes unless you set `timeout_s`, `--timeout-s` or `timeoutMs`. The console has no limit.

## Known limitations

- **Platforms.** Prebuilt for Linux (x86_64, arm64) and macOS (arm64, x86_64). Elsewhere, build from source and set `GITAGENT_BIN`.
- **The remote sandbox** uses 2.x's tools (`cli`, `read`, `write`, `edit`, `memory`), so background shell sessions and `write_stdin` are not available inside it. It runs through the npm command only.
- **Python.** The Python client is installed from this repository (`pip install ./clients/python`) and needs the binary.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/open-gitagent/gitagent/main/scripts/install.sh | sh
# or
npm install -g @open-gitagent/gitagent
```

The archives below hold the binary and the README for each platform. To check a download:

```sh
sha256sum -c SHA256SUMS --ignore-missing      # on macOS: shasum -a 256 -c SHA256SUMS
```

To build from source, see the README's [Install](../README.md#install) section.
