# Integrity

How gitagent keeps benchmark results honest, and how to check it.

## Rules and where they are enforced

| Rule | Enforced by |
|---|---|
| No reading or writing of hidden tests; nothing written under `/tests` | The tool dispatcher refuses file tools, patches and shell commands that would write under `/tests` in every benchmark run (`--bench`; outside benchmarks `/tests` is an ordinary path) (counter `protected_writes_blocked`). The system prompt also says so. Neither the agent loop nor the Harbor shim touches `/tests`. |
| No routing on task names, ids or hashes | Nothing in the code reads them. Behaviour comes only from the instruction, the environment and the profile. `scripts/integrity-audit.sh` checks for task names. |
| Benchmark runs depend only on committed files | `--bench` ignores project and home configuration (`gitagent.yaml`, prompts, profiles, skills, hooks, agent folders). Only the built-ins and explicit flags apply, and the Harbor shim uploads any profile it passes from the repository. A test fails if the prompt or tool list sent to the model in `--bench` changes. |
| No network except the model API in `--bench` | No web tools exist. Telemetry export is disabled in `--bench`, and repo mode is refused with it. The agent's own shell commands run inside the task container, under Harbor's network settings. |
| No memory or learning during a scored run | Each run starts with a fresh history and shell. Memory, online learning and lessons from a project live in configuration that `--bench` ignores. Lessons are learned offline, reviewed and committed; a scored run uses them unchanged or not at all. |
| Development tasks kept apart from scoring tasks | Changes are developed and measured on one set of tasks; the tasks used to score are kept apart, and their runs are not used to change the code. |
| No critic that rewrites answers | The stop check only accepts or rejects `task_complete`; it never changes the agent's work. |

## How certain the score is

The headline 93.03% ±0.8 is the mean of five runs over all 89 tasks; the ±0.8 is the standard error across the runs (93.3, 95.5, 92.1, 92.1 and 92.1%), the measure the official leaderboard reports. It does not cover a different question: how the score would hold on a different set of tasks. Resampling the 89 tasks with replacement 20,000 times gives a 95% range of 88.8–96.9%. It is wider because most tasks either always pass or always fail, so which tasks are in the set matters more than run-to-run variation. Against the highest published entry, 91.4%, the lead holds by the leaderboard's measure but sits inside this wider range.

## Audit

```
scripts/integrity-audit.sh            # task names from Harbor's local cache
scripts/integrity-audit.sh names.txt  # or from a file, one name per line
```

It checks prompts, profiles, `gitagent.yaml`, the source, the Harbor shim and the benchmark profiles and fixtures for:

1. any Terminal-Bench task name;
2. references to verifier code, reference solutions or test files (the `/tests` write guard and its tests, and the learner's reading of a run's reward, are the only expected matches);
3. benchmark-specific wording in the system prompt as sent to the model.
