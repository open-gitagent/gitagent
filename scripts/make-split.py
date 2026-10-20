#!/usr/bin/env python3
"""Creates bench/split.json: the permanent dev / held-out split of Terminal-Bench 2.1.

Stratified by category and difficulty from each task's task.toml metadata
(the only task file read), with a fixed seed. Tasks named in --force-dev go
into the dev split regardless (tasks whose trajectories were already read
during development must never be held out).

Usage:
  python3 scripts/make-split.py [--dev 45] [--seed 20261015] [--force-dev fix-git ...]

Refuses to overwrite an existing split: once committed it never changes.
"""

import argparse
import json
import random
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TASK_CACHE = Path.home() / ".cache" / "harbor" / "tasks" / "packages" / "terminal-bench"


def task_meta(task_dir: Path):
    for version in task_dir.iterdir():
        toml = version / "task.toml"
        if toml.is_file():
            text = toml.read_text()
            section = ""
            meta = {}
            for line in text.splitlines():
                line = line.split("#", 1)[0].strip()
                if line.startswith("["):
                    section = line.strip("[]")
                elif section == "metadata" and "=" in line:
                    k, _, v = line.partition("=")
                    meta[k.strip()] = v.strip().strip('"')
            return {"difficulty": meta.get("difficulty", "?"), "category": meta.get("category", "?")}
    return None


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dev", type=int, default=45)
    ap.add_argument("--seed", type=int, default=20261015)
    ap.add_argument("--force-dev", nargs="*", default=[])
    ap.add_argument("--out", type=Path, default=REPO / "bench" / "split.json")
    args = ap.parse_args()
    if args.out.exists():
        sys.exit(f"{args.out} exists; the split never changes once committed")

    tasks = {}
    for d in sorted(TASK_CACHE.iterdir()):
        if d.is_dir() and re.match(r"^[a-z0-9.-]+$", d.name):
            m = task_meta(d)
            if m:
                tasks[d.name] = m
    if len(tasks) < 80:
        sys.exit(f"only {len(tasks)} tasks in {TASK_CACHE}; download the dataset with harbor first")
    unknown = [t for t in args.force_dev if t not in tasks]
    if unknown:
        sys.exit(f"unknown tasks: {unknown}")

    # Stratum = category + difficulty. Each stratum contributes a dev share
    # proportional to its size; the remainder is settled by the seeded order.
    rng = random.Random(args.seed)
    strata = {}
    for name, m in tasks.items():
        strata.setdefault((m["category"], m["difficulty"]), []).append(name)
    dev = set(args.force_dev)
    dev_share = args.dev / len(tasks)
    leftovers = []
    for key in sorted(strata):
        names = sorted(strata[key])
        rng.shuffle(names)
        forced = [n for n in names if n in dev]
        quota = dev_share * len(names)
        want = int(quota)
        leftovers.append((quota - want, key, [n for n in names if n not in dev]))
        for n in names:
            if len(forced) + (0 if n in dev else 0) >= want:
                break
            if n not in dev:
                dev.add(n)
                forced.append(n)
    # Fill to exactly --dev from the strata with the largest fractional remainders.
    leftovers.sort(key=lambda x: -x[0])
    for _, key, names in leftovers:
        if len(dev) >= args.dev:
            break
        for n in names:
            if n not in dev:
                dev.add(n)
                break
    while len(dev) > args.dev:
        # Over quota only if --force-dev exceeded it; drop a random non-forced task.
        candidates = sorted(n for n in dev if n not in args.force_dev)
        dev.remove(rng.choice(candidates))

    heldout = sorted(set(tasks) - dev)
    split = {
        "dataset": "terminal-bench/terminal-bench-2-1",
        "method": "stratified by task.toml category and difficulty; seed %d; forced dev: %s" % (args.seed, ", ".join(args.force_dev) or "none"),
        "dev": sorted(dev),
        "heldout": heldout,
    }
    args.out.write_text(json.dumps(split, indent=2) + "\n")

    def dist(names):
        out = {}
        for n in names:
            out[tasks[n]["difficulty"]] = out.get(tasks[n]["difficulty"], 0) + 1
        return dict(sorted(out.items()))

    print(f"wrote {args.out}: {len(dev)} dev / {len(heldout)} held-out of {len(tasks)} tasks")
    print("dev difficulty:", dist(dev), " held-out difficulty:", dist(heldout))
    cats = sorted({m['category'] for m in tasks.values()})
    for c in cats:
        nd = sum(1 for n in dev if tasks[n]['category'] == c)
        nh = sum(1 for n in heldout if tasks[n]['category'] == c)
        print(f"  {c:28s} dev {nd:2d}  held-out {nh:2d}")


if __name__ == "__main__":
    main()
