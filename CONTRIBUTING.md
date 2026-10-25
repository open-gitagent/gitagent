# Contributing to gitagent

Thanks for helping. This guide covers gitagent 2, the Rust binary on this branch.

## Getting started

1. Fork the repository and clone your fork:
   ```sh
   git clone https://github.com/<your-username>/gitagent.git
   cd gitagent
   ```
2. Install Rust. The toolchain is pinned in `rust-toolchain.toml`, so `rustup` picks the right version on its own.
3. Build and test:
   ```sh
   cargo build
   cargo test --workspace
   ```

The tests need no network and no API keys. A scripted provider replays model replies from YAML (`--model scripted:<file>`), and the provider adapters are tested against local mock servers. To try a change by hand:

```sh
cargo run -p gitagent-cli -- -d path/to/agent -m scripted:path/to/script.yaml
```

## Before you open a pull request

Run all four; CI runs the same:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/integrity-audit.sh
```

- **The benchmark guard must pass.** `bench_prompt_and_tools_match_the_published_run` fails if the system prompt or the tool list sent to the model in `--bench` mode changes at all. Features for agent folders, the console or the SDKs must never reach benchmark mode. If a change to the benchmark setup is intended, say so in the pull request; it needs a measured run before it is merged.
- **Behaviour that changes results is a profile switch.** Add it to `profiles/*.yaml`, off by default, and describe it in `docs/HOW-IT-WORKS.md`.
- **Agents written for gitagent 1.x must keep working.** Tool names, hook scripts, `agent.yaml` fields and SDK options from 1.x are accepted; if you change how one is read, keep the 1.x form working and cover it with a test.

## Project structure

The workspace is split into crates under `crates/`:

| Crate | What it does |
|---|---|
| `cli` | The `gitagent` binary: flags, the console, the IPC protocol, subcommands (`init`, `validate`, `install`, `plugin`, `schedule`, `learn`) |
| `core` | The agent loop, sub-agents, the `plan` tool, workflows, approvals and the audit log |
| `loader` | Reads agent folders and configuration into one spec: `agent.yaml`, identity files, skills, tools, hooks, memory, knowledge, workflows, schedules, plugins, environments, compliance |
| `provider` | Model adapters: Anthropic, OpenAI, Google, OpenAI-compatible, and the scripted provider for tests |
| `tools` | Built-in tools: shell, files, edits, memory, skills |
| `terminal` | Persistent shells on a pseudo-terminal |
| `shaper` | Shapes command output before the model sees it |
| `context` | Masking and compaction of the conversation |
| `guards` | The stop check, the time budget and the loop detector |
| `hooks` | Hook scripts and their protocol |
| `mcp` | The Model Context Protocol client |
| `record` | Trajectories, counters and events |
| `repo` | Repo mode: clones, branches, commits, pushes |
| `sandbox` | Landlock and seccomp for the agent's shell |
| `learn` | Offline learning of lessons from finished runs |
| `sdk` | The Rust library interface |

The Python and TypeScript clients live under `clients/` and talk to the binary over the IPC protocol (`gitagent --ipc`).

## Guidelines

- **No unsafe code.** The workspace forbids it.
- **Small dependencies.** Prefer a few lines of code to a new crate.
- **Test what you change.** Unit tests next to the code, end-to-end tests in `crates/cli/tests/`.
- **One concern per pull request.** Keep it focused and reviewable.
- **Write docs in plain language.** Short sentences, one idea each.

## Commit messages

A short subject saying what changed, then a body saying why when it is not obvious:

- `Parallel tasks: the plan tool and a live task board`
- `Hooks that crash are a warning, as in 1.x`
- `Repo mode reuses an earlier clone of the same repository`

## Reporting issues

Search the existing issues first. Include what you ran, what you expected, what happened, `gitagent --version`, and your operating system. A trajectory (`trajectory.json` from the run's output folder) helps most.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](./LICENSE).
