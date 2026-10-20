# Integrity

How gitagent keeps benchmark results honest, and how to check it.

## Rules and where they are enforced

| Rule | Enforced by |
|---|---|
| No reading or writing of hidden tests; nothing written under `/tests` | The tool dispatcher refuses file tools, patches and shell commands that would write under `/tests`, in every mode (counter `protected_writes_blocked`). The system prompt also says so. Neither the agent loop nor the Harbor shim touches `/tests`. |
| No routing on task names, ids or hashes | Nothing in the code reads them. Behavior comes only from the instruction, the environment and the profile. `scripts/integrity-audit.sh` checks for task names. |
| Benchmark runs depend only on committed files | `--bench` ignores project and home config (`gitagent.yaml`, prompts, profiles, skills, hooks). Only the built-ins and explicit flags apply, and the Harbor shim uploads any profile it passes from the repository. |
| No network except the model API in `--bench` | No web tools exist. Telemetry export is disabled in `--bench`, and repo mode is refused with it. The agent's own shell commands run inside the task container, under Harbor's network settings. |
| No memory across tasks in `--bench` | Each run starts with a fresh history and shell. Interactive mode's follow-up tasks only continue within one session, and that mode is not available with `--bench`. Online learning (`learning: online`) lives in project config, which `--bench` ignores, and it never runs in `--bench`. |
| Learning only from dev tasks, frozen for held-out | `gitagent learn` reads only gitagent's own trajectories and each trial's reward, and only for tasks in the `dev` list of `bench/split.json`. Lessons that name the task or repeat its paths, identifiers, numbers or code are rejected. The result is a reviewed, committed `prompts/lessons.md`, which held-out runs use unchanged; nothing learns during a benchmark run. |
| No critic that rewrites answers | The stop check only accepts or rejects `task_complete`; it never changes the agent's work. |
| Held-out tasks unseen | `bench/split.json` (once created) stays fixed. Tasks whose trajectories were read during development (`fix-git`, `kv-store-grpc`) go in the dev split. |

## Audit

```
scripts/integrity-audit.sh            # task names from Harbor's local cache
scripts/integrity-audit.sh names.txt  # or from a file, one name per line
```

It checks prompts, profiles, `gitagent.yaml`, the source, the Harbor shim and the benchmark profiles and fixtures for:

1. any Terminal-Bench task name;
2. references to verifier code, reference solutions or test files (the `/tests` write guard and its tests, and the learner's reading of the reward from Harbor's `result.json`, are the only expected matches);
3. benchmark-specific wording in the system prompt as sent to the model (attribution comments are stripped before sending).

### Results

| Date | Commit | Task names checked | Result |
|---|---|---|---|
| 2026-10-14 | (this commit) | 89 (Terminal-Bench 2.1) | pass: no task names, no verifier or solution references, nothing benchmark-specific in the prompt |
| 2026-10-15 | (learner commit) | 89 (Terminal-Bench 2.1) | pass, after the learner was added |
