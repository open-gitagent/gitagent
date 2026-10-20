# How gitagent learns lessons

gitagent improves between runs by turning its own finished attempts into short,
general lessons. A lesson reaches the agent only after it has been seen to hold on
at least two different tasks, a person has read the change, and the binary has
been rebuilt. Nothing is learned during a benchmark run.

## The flow

```
 Harbor job (dev tasks only)
   └─ each trial: agent/trajectory.json + the reward in result.json
          │   (never tests, verifier output or reference solutions)
          ▼
 gitagent learn --from bench/jobs/<job> ...
   1. skip trials of tasks not in bench/split.json's "dev" list, and trials already learned from
   2. group the trials of one task into units: one passed and one failed attempt side by side when both exist, the rest one at a time
   3. show the learner model (prompts/learner.md) the unit and the current lesson list;
      it answers with at most 3 operations: add, edit, upvote, downvote
   4. leakage audit: reject any added or edited text that carries task details
   5. apply the rest to the store
          ▼
 bench/lessons/lessons.json        ← the store: every lesson, with its evidence
          ▼  render: lessons backed by ≥ 2 different tasks, by importance, up to 3,000 characters
 prompts/lessons.md                ← the lessons the agent will see (review this diff, then commit)
          ▼  scripts/build-release.sh  (the file is compiled into the binary)
 target/*/release/gitagent         ← appended to the system prompt as "# Lessons from earlier tasks"
```

## Where lessons live

| File | What it holds | Who reads it |
|---|---|---|
| `bench/lessons/lessons.json` | Every lesson ever kept: text, importance, helpful/harmful counts, the **tasks** that added or confirmed it, and the **trials** it came from | `gitagent learn`, people reviewing |
| `prompts/lessons.md` | Only the lessons that passed the threshold, as a bullet list | Compiled into the binary; the agent sees it |
| `prompts/learner.md` | The learner model's instructions | `gitagent learn` |

## The four operations

The learner model sees one or two attempts at a task, whether the hidden checker
accepted them, and the current lessons. It may:

| Operation | Effect on the store |
|---|---|
| `add` | New lesson, importance 2, `tasks` = this task. Skipped if it is ≥ 80% the same words as an existing lesson. |
| `edit` | Rewrites a lesson to be more accurate or general; counts as a vote from this task. |
| `upvote` | The attempt followed the lesson and it helped, or shows it is right: `helpful` + 1, vote from this task. |
| `downvote` | The attempt shows the lesson hurt or is wrong: importance − 1, `harmful` + 1. At importance 0 the lesson is deleted. |

A vote from a task raises importance **only the first time that task votes**, and
adds the task to the lesson's `tasks` list. Running the same task many times
therefore cannot make a lesson look widely supported.

## The leakage audit

Every added or edited lesson is checked before it is stored. It is rejected if it
contains the task's name, a path or file name, a quoted identifier or a number
taken from the instruction, code, or is too long to be general advice (over 400
characters; the learner is asked for at most 300). The learner is also told never to mention the task,
its files, ports, commands that only fit it, or its answer. Rejections are
counted in the learner's report (`ops_rejected`).

## Which lessons reach the agent

`prompts/lessons.md` is rebuilt from the store every time `gitagent learn` runs:

1. keep lessons whose `tasks` list has **at least 2 different tasks** (`--min-tasks`, default 2);
2. sort them by **importance**, highest first;
3. add them one by one until the file would pass **3,000 characters** (`--max-chars`).

A lesson seen on one task stays in the store, waiting for a second task to confirm
it. A lesson that is eligible but falls past the size limit also stays in the
store and can come back if its importance rises. The limit keeps the system
prompt short, and the same lessons go to every task, so the prompt stays
byte-identical and cached.

## When lessons take effect

The lessons are compiled into the binary, and Harbor uploads only the binary into
each task container. A change to `prompts/lessons.md` therefore reaches benchmark
runs only after:

1. the diff of `prompts/lessons.md` has been read (no task details, sensible advice);
2. it is committed;
3. both binaries are rebuilt with `scripts/build-release.sh`.

Outside benchmarks, `learning: online` in `gitagent.yaml` learns after each finished
task into the project's `.gitagent/prompts/lessons.md`, which later runs load on
top of the built-in lessons. It is never active with `--bench`.

## Rules

- Learn from dev tasks only. `gitagent learn` refuses trials of tasks outside
  `bench/split.json`'s `dev` list, so held-out runs can never feed the lessons.
- The learner reads only gitagent's own trajectory and the reward; never tests,
  verifier output or reference solutions.
- A lesson's support is the learner model's judgment that an attempt shows it, not
  a measured cause. Whether the lessons help is measured separately, by running
  rounds with and without them.

## History so far

| When | From | Store | In the prompt | Notes |
|---|---|---|---|---|
| Passes 1–4 (round r0, on the Mac) | the passes' dev trials | 53 | 9 | Learned after each pass; first lessons shown once a second task supported them |
| After round r1, run 1 (b15bb30) | 45 trials | 84 | 10 | 31 added, 72 upvoted, 10 edited, 2 rejected by the audit |
| After round r1, runs 2–3 (98b89c4) | 90 trials | 107 | 10 | 23 added, 183 upvoted, 16 edited, none rejected |
| Round r2 | — | — | 10 | Measured with the new lessons: 93.3% against r1's 94.1%, the same within noise |

The held-out run uses r2's lessons, frozen; nothing is learned from it.
