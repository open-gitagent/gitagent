#!/usr/bin/env python3
"""A local dashboard over gitagent's benchmark runs.

Serves a single-page site that watches the Harbor job directories live:
jobs and trials, accuracy per task, tokens, turns, every counter with its
meaning, why each trial failed, the guard and loop events in a trajectory,
the commands and logs of a trial, the lesson store, the task catalog, and
which features each profile switches on.

It reads only gitagent's own files, the reward in each trial's result.json
and each task's task.toml metadata; it never opens a task's verifier
output, tests or solutions.

Usage:
  python3 scripts/dashboard.py [--root bench/jobs] [--port 8731]

Stdlib only (Python 3.9+); binds to localhost only.
"""

import argparse
import fnmatch
import json
import os
import re
import subprocess
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

REPO = Path(__file__).resolve().parent.parent
TASK_CACHE = Path.home() / ".cache" / "harbor" / "tasks" / "packages" / "terminal-bench"
SAFE_NAME = re.compile(r"^[A-Za-z0-9._-]+$")
SAFE_REL = re.compile(r"^(commands/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+|out/[A-Za-z0-9._-]+|gitagent\.log|setup/[A-Za-z0-9._-]+)$")
# A trial with no result whose files stopped changing this long ago is
# treated as abandoned rather than running.
RUNNING_WINDOW_S = 900
# A running trial past this estimated spend is flagged in the header (a model or agent loop).
RUNAWAY_USD = 5.0
STEP_TEXT_CAP = 20_000
LOG_TAIL_LINES = 400
FILE_CAP = 256 * 1024


# ---------------------------------------------------------------- helpers

def load_json(path: Path):
    try:
        with path.open() as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError):
        return None


def read_text(path: Path, cap=FILE_CAP):
    try:
        data = path.read_bytes()
    except OSError:
        return None
    if len(data) > cap:
        data = data[-cap:]
    return data.decode("utf-8", "replace")


def parse_iso(text):
    if not text:
        return None
    try:
        return datetime.fromisoformat(str(text).replace("Z", "+00:00"))
    except ValueError:
        return None


def seg_seconds(result, key):
    seg = (result or {}).get(key) or {}
    a, b = parse_iso(seg.get("started_at")), parse_iso(seg.get("finished_at"))
    if a and b:
        return round((b - a).total_seconds(), 1)
    return None


def mtime_iso(path: Path):
    try:
        return datetime.fromtimestamp(path.stat().st_mtime, timezone.utc).isoformat()
    except OSError:
        return None


def cap(text, limit=STEP_TEXT_CAP):
    text = str(text)
    if len(text) <= limit:
        return text
    return text[:limit] + f"\n… [{len(text) - limit} more characters]"


def toml_lite(text: str):
    """Enough TOML for task.toml: sections, scalars and one-line arrays."""
    out_root, section = {}, None
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[["):
            section = {}  # arrays of tables are not needed; swallow their keys
            continue
        if line.startswith("["):
            section = out_root.setdefault(line.strip("[]").strip(), {})
            continue
        if "=" not in line:
            continue
        key, _, value = line.partition("=")
        key, value = key.strip(), value.strip()
        target = section if section is not None else out_root
        if value.startswith("["):
            target[key] = re.findall(r'"([^"]*)"', value)
        elif value.startswith('"'):
            target[key] = value.strip('"')
        elif value in ("true", "false"):
            target[key] = value == "true"
        else:
            try:
                target[key] = float(value) if "." in value else int(value)
            except ValueError:
                target[key] = value
    return out_root


def yaml_flags(text: str):
    """Scalar `path: value` pairs from a simple YAML file (lists skipped)."""
    flags, stack = {}, []
    for raw in text.splitlines():
        line = raw.split(" #", 1)[0].rstrip()
        if not line.strip() or line.lstrip().startswith(("#", "-")):
            continue
        indent = len(line) - len(line.lstrip())
        key, _, value = line.strip().partition(":")
        value = value.strip()
        while stack and stack[-1][0] >= indent:
            stack.pop()
        path = ".".join([s[1] for s in stack] + [key])
        if value.startswith("{"):
            for pair in value.strip("{}").split(","):
                k, _, v = pair.partition(":")
                if k.strip():
                    flags[f"{path}.{k.strip()}"] = v.strip()
        elif value == "":
            stack.append((indent, key))
        else:
            flags[path] = value
    return flags


_pricing_cache = {"at": 0.0, "prices": {}}


def pricing():
    if time.time() - _pricing_cache["at"] > 60:
        prices = load_json(REPO / "bench" / "pricing.json") or {}
        _pricing_cache.update(at=time.time(), prices={k: v for k, v in prices.items() if not k.startswith("_")})
    return _pricing_cache["prices"]


# Display names per bare model id: (label, short label, full name as the public
# leaderboards write it, provider). Unknown ids fall back to the id itself.
MODEL_NAMES = {
    "claude-opus-5-5": ("Opus 5.5", "Opus 5.5", "Claude Opus 5.5", "anthropic"),
    "claude-opus-5": ("Opus 5", "Opus 5", "Claude Opus 5", "anthropic"),
    "gpt-6-astra": ("GPT-6 Astra", "GPT-6", "GPT-6 Astra", "openai"),
}


def model_id(name):
    """The bare model id ("claude-opus-5-5") from any spelling the files use:
    "anthropic/claude-opus-5-5" (config.json), "anthropic:claude-opus-5-5"
    (trajectory) or the bare id itself (result.json)."""
    if not name:
        return None
    return str(name).split("/")[-1].split(":")[-1].strip() or None


def model_provider(name):
    """The provider part of a model name ("anthropic", "openai"), or the one
    known for the bare id, or None."""
    if not name:
        return None
    s = str(name).strip()
    for sep in ("/", ":"):
        if sep in s:
            return s.split(sep, 1)[0].strip().lower() or None
    return MODEL_NAMES.get(model_id(s), (None, None, None, None))[3]


def model_info(mid, index, has_rounds=False):
    label, short, full, provider = MODEL_NAMES.get(mid, (mid, mid, mid, None))
    return {"id": mid, "label": label, "short": short, "full": full, "provider": provider,
            "index": index, "priced": mid in pricing(), "has_rounds": has_rounds}


def cost_usd(model, input_tokens, output_tokens, cache_read, cache_write):
    """Estimated cost from bench/pricing.json, or None when the model has no price."""
    name = model_id(model)
    if not name:
        return None
    price = pricing().get(name)
    if not price:
        return None
    uncached = max(0, (input_tokens or 0) - (cache_read or 0) - (cache_write or 0))
    usd = (uncached * price["input"] + (cache_read or 0) * price["cache_read"]
           + (cache_write or 0) * price["cache_write"] + (output_tokens or 0) * price["output"]) / 1e6
    return round(usd, 4)


def trajectory_usage(trial_dir: Path):
    """Token totals from an ATIF agent/trajectory.json (written live by Terminus 2),
    as (input, output, cached), or None when there is none yet."""
    traj = load_json(trial_dir / "agent" / "trajectory.json")
    if not isinstance(traj, dict):
        return None
    fm = traj.get("final_metrics") or {}
    if fm.get("total_prompt_tokens") is not None:
        return fm.get("total_prompt_tokens"), fm.get("total_completion_tokens"), fm.get("total_cached_tokens")
    steps = [s.get("metrics") or {} for s in traj.get("steps") or [] if isinstance(s, dict)]
    if not any(steps):
        return None
    return (sum(m.get("prompt_tokens") or 0 for m in steps), sum(m.get("completion_tokens") or 0 for m in steps),
            sum(m.get("cached_tokens") or 0 for m in steps))


def agent_kind(name):
    n = (name or "").lower()
    if "oracle" in n:
        return "oracle"
    if "gitagent" in n:
        return "gitagent"
    if "terminus" in n:
        return "terminus"
    return n or "unknown"


# ---------------------------------------------------------------- task catalog

_live_cache = {"at": 0.0, "names": None}


def live_trials():
    """Lower-cased names of trials whose task container is up, from every
    Docker context, or None when Docker cannot be asked."""
    if time.time() - _live_cache["at"] < 5:
        return _live_cache["names"]
    names = None
    try:
        contexts = subprocess.run(["docker", "context", "ls", "-q"], capture_output=True, text=True, timeout=5).stdout.split()
        names = set()
        for ctx in contexts or [""]:
            cmd = ["docker"] + (["--context", ctx] if ctx else []) + ["ps", "--format", "{{.Names}}"]
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=5)
            if out.returncode != 0:
                # No access to Docker (e.g. not in the docker group): unknown, not "nothing is up".
                raise OSError(out.stderr.strip())
            for line in out.stdout.split():
                if "__env" in line:
                    names.add(line.split("__env")[0].lower())
    except (OSError, subprocess.SubprocessError):
        names = None
    _live_cache.update(at=time.time(), names=names)
    return names


_catalog_cache = {"at": 0.0, "tasks": {}}


def task_catalog():
    """task.toml metadata for every task in Harbor's local cache."""
    if time.time() - _catalog_cache["at"] < 300:
        return _catalog_cache["tasks"]
    tasks = {}
    if TASK_CACHE.is_dir():
        for task_dir in sorted(TASK_CACHE.iterdir()):
            if not task_dir.is_dir():
                continue
            toml_path = None
            try:
                for version in task_dir.iterdir():
                    cand = version / "task.toml"
                    if cand.is_file():
                        toml_path = cand
            except OSError:
                pass
            if toml_path is None:
                continue
            text = read_text(toml_path) or ""
            t = toml_lite(text)
            meta, task, env = t.get("metadata", {}), t.get("task", {}), t.get("environment", {})
            tasks[task_dir.name] = {
                "task": task_dir.name,
                "description": task.get("description", ""),
                "difficulty": meta.get("difficulty", "?"),
                "category": meta.get("category", "?"),
                "tags": meta.get("tags") or task.get("keywords") or [],
                "expert_min": meta.get("expert_time_estimate_min"),
                "agent_timeout_s": t.get("agent", {}).get("timeout_sec"),
                "verifier_timeout_s": t.get("verifier", {}).get("timeout_sec"),
                "cpus": env.get("cpus"),
                "memory_mb": env.get("memory_mb"),
                "internet": env.get("allow_internet"),
            }
    _catalog_cache.update(at=time.time(), tasks=tasks)
    return tasks


# ---------------------------------------------------------------- trials

HARBOR_ERRORS = {
    "VerifierTimeoutError": "The checker ran out of time. Usually the machine is too slow (emulation) or a service it needed was not up.",
    "AgentTimeoutError": "Harbor stopped the agent at the task's time limit before it finished.",
    "EnvironmentBuildTimeoutError": "The task's Docker image did not build in time.",
    "AgentSetupTimeoutError": "Installing the agent into the container took too long.",
}

OUTCOMES = {
    "completed": "The agent called task_complete and it was accepted.",
    "max_turns": "The agent used every turn allowed without finishing.",
    "time_up": "gitagent's own time budget ran out before task_complete.",
    "no_tool_calls": "The model answered three times in a row without calling a tool.",
    "refused": "The model's safety classifiers declined to continue.",
}


def diagnose(row, counters):
    """A plain-language reason for the trial's result, plus notes."""
    status, outcome, exc = row["status"], row["outcome"], row["exception"]
    notes = []
    c = counters or {}

    def note(key, text):
        if c.get(key):
            notes.append(f"{text} ({key} = {c[key]})")

    note("loop_triggers", "The loop detector fired: the same action gave the same result repeatedly")
    note("stop_rejections", "task_complete was rejected because nothing was verified after an edit")
    note("compactions", "The history was compacted to fit the context window")
    note("overflow_compactions", "The provider reported a context overflow; the history was compacted and the turn retried")
    note("nudges", "A time-budget reminder was sent")
    note("forced_stops", "The time budget forced a final call")
    note("truncated_turns", "A reply hit the output token limit")
    note("no_tool_turns", "Replies without a tool call needed a nudge")
    note("repairs", "Tool calls had to be parsed from plain text")
    note("refusals", "The model refused")
    note("fallback_turns", "A fallback model served some turns")
    note("protected_writes_blocked", "A write under /tests was refused")
    note("exec_timeouts", "Commands kept running past their wait and returned early")
    note("shell_restarts", "The shell had to be restarted")
    note("masks_applied", "Old tool outputs were hidden to save context")
    note("subagent_calls", "Sub-agents were used")

    if status == "running":
        return {"reason": "Still running.", "notes": notes}
    if status == "checking":
        return {"reason": "The agent has finished; Harbor is running the task's checker.", "notes": notes}
    if status == "abandoned":
        return {"reason": "No result was written and the files stopped changing; the job was probably interrupted.", "notes": notes}
    if status == "error":
        msg = f" Harbor said: {row['exception_message']}" if row.get("exception_message") else ""
        return {"reason": f"Harbor error {exc}: {HARBOR_ERRORS.get(exc, 'the run did not get a verdict.')}{msg}", "notes": notes}
    if status == "pass":
        return {"reason": "The checker accepted the result.", "notes": notes}
    if row["kind"] == "oracle":
        return {
            "reason": "The task's own reference solution did not pass its checker, so the machine is at fault (too slow under emulation, too little memory, or a service that did not come up), not an agent.",
            "notes": notes,
        }
    if outcome and outcome != "completed":
        base = OUTCOMES.get(outcome.split(":")[0], outcome)
        if outcome.startswith("provider_failed"):
            base = f"The model API kept failing: {outcome.split(':', 1)[-1].strip()}"
        return {"reason": base, "notes": notes}
    if status == "fail":
        return {
            "reason": "The agent declared the task complete, but the checker rejected the result. Read the last turns to see what it verified and what it missed.",
            "notes": notes,
        }
    if status == "no_verdict":
        return {"reason": "The run finished but no reward was recorded.", "notes": notes}
    return {"reason": "Unknown.", "notes": notes}


def last_action(trial_dir: Path):
    """What the agent was doing last, from the trajectory tail."""
    traj = load_json(trial_dir / "agent" / "trajectory.json") or {}
    steps = traj.get("steps") or []
    for step in reversed(steps):
        if step.get("source") != "agent":
            continue
        calls = step.get("tool_calls") or []
        if calls:
            c = calls[-1]
            args = c.get("arguments") or {}
            detail = args.get("cmd") or args.get("path") or args.get("summary") or ""
            return {"step": step.get("step_id"), "text": f"{c.get('function_name')} {cap(detail, 160)}".strip()}
        return {"step": step.get("step_id"), "text": cap(step.get("message") or "", 160)}
    return {"step": len(steps), "text": "(no agent step yet)"}


def agent_turns(trial_dir: Path):
    """Model calls in a trajectory (for agents that do not report turns)."""
    traj = load_json(trial_dir / "agent" / "trajectory.json") or {}
    steps = [st for st in traj.get("steps") or [] if st.get("source") == "agent"]
    return len(steps) or None


def newest_mtime(trial_dir: Path):
    newest = 0.0
    for p in (
        trial_dir / "agent" / "counters.json",
        trial_dir / "agent" / "trajectory.json",
        trial_dir / "agent" / "gitagent.log",
        trial_dir / "trial.log",
        trial_dir / "config.json",
    ):
        try:
            newest = max(newest, p.stat().st_mtime)
        except OSError:
            pass
    return newest


TIMEOUT_MULTIPLIERS = ("timeout_multiplier", "agent_timeout_multiplier", "verifier_timeout_multiplier",
                       "agent_setup_timeout_multiplier", "environment_build_timeout_multiplier")
TIMEOUT_OVERRIDES = (("agent", "override_timeout_sec"), ("agent", "max_timeout_sec"), ("agent", "override_setup_timeout_sec"),
                     ("verifier", "override_timeout_sec"), ("verifier", "max_timeout_sec"))


def effective_multipliers(raw):
    """Each timeout multiplier as Harbor applies it: an unset per-phase one
    falls back to timeout_multiplier (itself 1.0 when unset)."""
    if not raw:
        return None
    base = raw.get("timeout_multiplier")
    base = 1.0 if base is None else base
    return {k: (base if raw.get(k) is None else raw[k]) for k in TIMEOUT_MULTIPLIERS}


def trial_harbor(result, cfg):
    """What Harbor recorded about how a trial was run: the task's version and
    checksum, the verifier mode and the timeout settings. The settings come
    from result.json's config, or the trial's config.json while it runs."""
    result = result if isinstance(result, dict) else {}
    tcfg = result.get("config") if isinstance(result.get("config"), dict) else (cfg if isinstance(cfg, dict) else {})
    task_id = result.get("task_id") if isinstance(result.get("task_id"), dict) else {}
    raw = {k: tcfg.get(k) for k in TIMEOUT_MULTIPLIERS} if tcfg else None
    overrides = {f"{sec}.{key}": (tcfg.get(sec) or {}).get(key) for sec, key in TIMEOUT_OVERRIDES
                 if isinstance(tcfg.get(sec), dict) and (tcfg.get(sec) or {}).get(key) is not None}
    return {
        "task_checksum": result.get("task_checksum"),
        "task_id": {k: task_id.get(k) for k in ("org", "name", "ref")} if task_id else None,
        "source": result.get("source"),
        "verifier_environment_mode": result.get("verifier_environment_mode"),
        "timeout_multipliers": raw,
        "effective_multipliers": effective_multipliers(raw),
        "timeout_overrides": overrides,
    }


def read_trial(trial_dir: Path, with_live=True, job_running=False):
    """One row per trial, from result.json and gitagent's own agent/ files."""
    result = load_json(trial_dir / "result.json")
    cfg = load_json(trial_dir / "config.json") or {}
    agent_cfg = (cfg.get("agent") or {}) if isinstance(cfg, dict) else {}

    task = (result or {}).get("task_name")
    if not task and isinstance(cfg, dict):
        task = (cfg.get("task") or {}).get("name")
    task = (task or trial_dir.name.split("__")[0]).rsplit("/", 1)[-1]

    # Built-in Harbor agents (oracle, terminus) are named only in the result.
    agent_info = (result or {}).get("agent_info") or {}
    agent_name = agent_cfg.get("name") or agent_info.get("name")
    # The model as the job was configured ("anthropic/claude-opus-5-5"); when the
    # config has none, the result's model_info ("claude-opus-5-5" + provider).
    minfo = agent_info.get("model_info") or {}
    model_raw = agent_cfg.get("model_name") or (
        f"{minfo['provider']}/{minfo['name']}" if minfo.get("name") and minfo.get("provider") else minfo.get("name"))
    row = {
        "trial": trial_dir.name,
        "task": task,
        "agent": agent_name,
        "kind": agent_kind(agent_name),
        "model": model_raw,
        "model_id": model_id(model_raw),
        "provider": model_provider(model_raw),
        "profile": Path(str((agent_cfg.get("kwargs") or {}).get("profile", ""))).stem or None,
        "reward": None,
        "outcome": None,
        "status": "running",
        "exception": None,
        "exception_message": None,
        "turns": None,
        "input_tokens": None,
        "output_tokens": None,
        "cache_tokens": None,
        "agent_s": None,
        "verifier_s": None,
        "started": mtime_iso(trial_dir / "config.json"),
        "elapsed_s": None,
        "live": None,
        "counters": None,
        "cost_usd": None,
        # Harbor's per-trial record (task version, timeout settings); scan() and
        # trial_detail() take it out of the row before it is sent.
        "_harbor": trial_harbor(result, cfg),
    }

    if result is None:
        counters = load_json(trial_dir / "agent" / "counters.json") or {}
        row["counters"] = counters
        row["turns"] = counters.get("turns")
        row["input_tokens"] = counters.get("input_tokens")
        row["output_tokens"] = counters.get("output_tokens")
        row["cache_tokens"] = counters.get("cache_read_tokens")
        if row["input_tokens"] is None:
            # Not gitagent (no counters.json): read live usage from the agent's ATIF trajectory.
            usage = trajectory_usage(trial_dir)
            if usage:
                row["input_tokens"], row["output_tokens"], row["cache_tokens"] = usage
        row["cost_usd"] = cost_usd(row["model"], row["input_tokens"], row["output_tokens"], row["cache_tokens"], counters.get("cache_write_tokens"))
        mtime = newest_mtime(trial_dir)
        started = parse_iso(row["started"])
        if started:
            row["elapsed_s"] = round(time.time() - started.timestamp())
        # Once gitagent has written its done file, Harbor is running the checker.
        live = live_trials()
        # Compose project names turn "." into "-" (install-windows-3.11 → install-windows-3-11).
        alive = trial_dir.name.lower().replace(".", "-") in live if live is not None else None
        if (trial_dir / "agent" / "done").is_file():
            row["status"] = "checking"
        elif alive is False or (alive is None and not job_running and mtime and time.time() - mtime > RUNNING_WINDOW_S):
            row["status"] = "abandoned"
        elif with_live:
            row["live"] = last_action(trial_dir)
            row["live"]["idle_s"] = round(time.time() - mtime) if mtime else None
        row["diagnosis"] = diagnose(row, counters)
        return row

    agent_result = result.get("agent_result") or {}
    meta = agent_result.get("metadata") or {}
    counters = meta.get("counters") or {}
    rewards = (result.get("verifier_result") or {}).get("rewards") or {}
    exc = result.get("exception_info")

    row["counters"] = counters
    row["reward"] = rewards.get("reward")
    row["outcome"] = meta.get("outcome")
    row["exception"] = (exc or {}).get("exception_type") if isinstance(exc, dict) else exc
    row["exception_message"] = (exc or {}).get("exception_message") if isinstance(exc, dict) else None
    row["turns"] = counters.get("turns")
    row["input_tokens"] = agent_result.get("n_input_tokens")
    row["output_tokens"] = agent_result.get("n_output_tokens")
    row["cache_tokens"] = agent_result.get("n_cache_tokens")
    # Price every run from bench/pricing.json so all harnesses use the same prices
    # (Terminus's client library reports Opus 5.5 at the older Opus 5 rates). Its
    # own figure is kept only for models the file does not price.
    reported = agent_result.get("cost_usd")
    estimated = cost_usd(row["model"], row["input_tokens"], row["output_tokens"], row["cache_tokens"], counters.get("cache_write_tokens"))
    use_reported = estimated is None and isinstance(reported, (int, float)) and reported > 0
    row["cost_usd"] = round(reported, 4) if use_reported else estimated
    row["cost_source"] = "reported" if use_reported else "estimated"
    row["reported_cost_usd"] = round(reported, 4) if isinstance(reported, (int, float)) else None
    if row["turns"] is None and row["kind"] != "oracle":
        row["turns"] = agent_turns(trial_dir)
    row["agent_s"] = seg_seconds(result, "agent_execution")
    row["verifier_s"] = seg_seconds(result, "verifier")
    row["started"] = result.get("started_at") or row["started"]
    if row["exception"]:
        row["status"] = "error"
    elif row["reward"] is None:
        row["status"] = "no_verdict"
    elif row["reward"] >= 1.0:
        row["status"] = "pass"
    else:
        row["status"] = "fail"
    row["diagnosis"] = diagnose(row, counters)
    return row


def trial_dirs(job_dir: Path):
    out = []
    try:
        for d in job_dir.iterdir():
            if d.is_dir() and (d / "config.json").is_file():
                out.append(d)
    except OSError:
        pass
    return sorted(out)


def load_rounds():
    data = load_json(REPO / "bench" / "rounds.json") or {}
    return [r for r in data.get("rounds") or [] if isinstance(r, dict) and r.get("name")]


def round_of(job_name, rounds):
    for r in rounds:
        if any(fnmatch.fnmatchcase(job_name, g) for g in r.get("jobs") or []):
            return r["name"]
    return None


# bench/hub.json (optional): {"<job name>": "<Harbor Hub URL>"}; `harbor upload` leaves nothing in the job dir once it succeeds, so the link is recorded there by hand.
HUB_FILE = REPO / "bench" / "hub.json"
# The .harbor-upload/ folder holds Harbor's upload archive only while an upload runs; one left behind means it did not finish.
UPLOAD_SCRATCH = ".harbor-upload"
# The difference, in points, between Harbor's mean reward and the dashboard's accuracy that gets a warning.
MEAN_TOLERANCE_PTS = 0.5


def hub_links():
    data = load_json(HUB_FILE)
    if not isinstance(data, dict):
        return {}
    return {k: v for k, v in data.items() if isinstance(v, str) and re.match(r"^https?://", v)}


def harbor_job(job_dir: Path, trials, metas, hub):
    """Harbor's own record of a job, from its result.json and config.json:
    counts, mean reward and pass@k per agent/model/dataset, and the settings
    the job ran with (Harbor leaves out settings at their default value)."""
    res = load_json(job_dir / "result.json")
    cfg = load_json(job_dir / "config.json")
    # Harbor writes the job's result.json while it runs; finished_at is set at the end.
    finished = isinstance(res, dict) and bool(res.get("finished_at"))
    res = res if finished else {}
    cfg = cfg if isinstance(cfg, dict) else {}
    stats = res.get("stats") or {}

    agents_cfg = cfg.get("agents")
    agents = []
    for a in agents_cfg if isinstance(agents_cfg, list) else [{}]:
        a = a if isinstance(a, dict) else {}
        kwargs = a.get("kwargs") if isinstance(a.get("kwargs"), dict) else {}
        agents.append({
            "name": a.get("import_path") or a.get("name") or "oracle",
            "model_name": a.get("model_name"),
            "model_id": model_id(a.get("model_name")),
            "kwargs": kwargs,
            "profile": kwargs.get("profile"),
            "timeout_s": kwargs.get("timeout_s"),
        })
    datasets = []
    for ds in cfg.get("datasets") or []:
        if isinstance(ds, dict):
            names = ds.get("task_names")
            datasets.append({"name": ds.get("name") or ds.get("path"), "ref": ds.get("ref"),
                             "n_tasks": len(names) if isinstance(names, list) and names else None})
    n_attempts = cfg.get("n_attempts")
    attempts = n_attempts if isinstance(n_attempts, int) else 1
    listed = [d["n_tasks"] for d in datasets]
    tasks_known = bool(listed) and all(n is not None for n in listed)
    n_tasks = sum(listed) if tasks_known else len({t["task"] for t in trials})

    evals = []
    for key, e in (stats.get("evals") or {}).items():
        e = e if isinstance(e, dict) else {}
        parts = key.split("__")
        agent, dataset = parts[0], parts[-1]
        model = parts[1] if len(parts) >= 3 else None
        mine = [t for t in trials if t["kind"] == agent_kind(agent) and (t["model_id"] or None) == (model or None)]
        if not mine and len(stats.get("evals") or {}) == 1:
            mine = trials
        scored = [t for t in mine if t["reward"] is not None]
        passed = sum(1 for t in scored if t["reward"] >= 1.0)
        mean = next((m.get("mean") for m in e.get("metrics") or [] if isinstance(m, dict) and m.get("mean") is not None), None)
        dash = passed / len(scored) if scored else None
        diff = round(100 * (mean - dash), 2) if mean is not None and dash is not None else None
        evals.append({
            "key": key, "agent": agent, "model": model, "dataset": dataset,
            "n_trials": e.get("n_trials"), "n_errors": e.get("n_errors"), "mean": mean,
            "pass_at_k": {str(k): v for k, v in sorted((e.get("pass_at_k") or {}).items(), key=lambda kv: int(kv[0]) if str(kv[0]).isdigit() else 0)},
            "exceptions": {k: len(v) if isinstance(v, list) else v for k, v in (e.get("exception_stats") or {}).items()},
            "dash_passed": passed, "dash_scored": len(scored), "dash_accuracy": dash,
            "diff_pts": diff, "differs": diff is not None and abs(diff) > MEAN_TOLERANCE_PTS,
        })

    multipliers, overrides, versions = {}, {}, {}
    for t, m in zip(trials, metas):
        for k, v in (m.get("effective_multipliers") or {}).items():
            multipliers.setdefault(k, set()).add(v)
        for k, v in (m.get("timeout_overrides") or {}).items():
            overrides.setdefault(k, set()).add(v)
        versions.setdefault(t["task"], set())
        if m.get("task_checksum"):
            versions[t["task"]].add(m["task_checksum"])
    count = lambda k: stats.get(k) if finished else None  # noqa: E731
    return {
        "id": res.get("id"),
        "finished": finished,
        "started_at": res.get("started_at"),
        "finished_at": res.get("finished_at"),
        "n_total_trials": res.get("n_total_trials"),
        "completed": count("n_completed_trials"),
        "errored": count("n_errored_trials"),
        "running": count("n_running_trials"),
        "pending": count("n_pending_trials"),
        "cancelled": count("n_cancelled_trials"),
        "retries": count("n_retries"),
        "n_attempts": attempts,
        "n_attempts_default": not isinstance(n_attempts, int),
        "n_concurrent_trials": cfg.get("n_concurrent_trials") if isinstance(cfg.get("n_concurrent_trials"), int) else 4,
        "n_concurrent_default": not isinstance(cfg.get("n_concurrent_trials"), int),
        "agents": agents,
        "agents_default": not isinstance(agents_cfg, list),
        "datasets": datasets,
        "n_tasks": n_tasks,
        "tasks_listed": tasks_known,
        "expected_trials": n_tasks * attempts * max(len(agents), 1),
        "present_trials": len(trials),
        "evals": evals,
        "multipliers": {k: sorted(v) for k, v in multipliers.items()},
        "timeout_overrides": {k: sorted(v) for k, v in overrides.items()},
        "task_versions": max((len(v) for v in versions.values()), default=0),
        "hub_url": hub.get(job_dir.name),
        "upload_unfinished": (job_dir / UPLOAD_SCRATCH).is_dir(),
        # Internal: the checksums seen per task, for the round's version check; summary() drops it.
        "_versions": {k: sorted(v) for k, v in versions.items()},
    }


def round_compliance(jobs):
    """The round's run settings against what a publishable result needs: the
    same trials per task everywhere, timeouts unscaled, nothing retried or
    errored, every expected trial present, and one version of each task."""
    if not jobs:
        return None
    hs = [j["harbor"] for j in jobs]
    mult, over, versions = {}, {}, {}
    for h in hs:
        for k, v in h["multipliers"].items():
            mult.setdefault(k, set()).update(v)
        for k, v in h["timeout_overrides"].items():
            over.setdefault(k, set()).update(v)
        for task, sums in h["_versions"].items():
            versions.setdefault(task, set()).update(sums)
    num = lambda k: sum(h[k] or 0 for h in hs)  # noqa: E731
    return {
        "jobs": len(hs),
        "unfinished_jobs": sorted(j["name"] for j in jobs if not j["harbor"]["finished"]),
        "n_attempts": sorted({h["n_attempts"] for h in hs}),
        "n_attempts_defaulted": sorted(j["name"] for j in jobs if j["harbor"]["n_attempts_default"]),
        "multipliers": {k: sorted(v) for k, v in mult.items()},
        "multipliers_ok": bool(mult) and all(x == 1.0 for v in mult.values() for x in v) and not over,
        "timeout_overrides": {k: sorted(v) for k, v in over.items()},
        "retries": num("retries"),
        "errored": num("errored"),
        "cancelled": num("cancelled"),
        "present": num("present_trials"),
        "expected": num("expected_trials"),
        "expected_exact": all(h["tasks_listed"] for h in hs),
        "harbor_total": num("n_total_trials"),
        "tasks": len(versions),
        "multi_version_tasks": sorted(t for t, v in versions.items() if len(v) > 1),
        "unversioned_tasks": sorted(t for t, v in versions.items() if not v),
        "max_versions": max((len(v) for v in versions.values()), default=0),
    }


def scan(root: Path):
    jobs = []
    rounds = load_rounds()
    round_model = {r["name"]: r.get("model") for r in rounds}
    hub = hub_links()
    try:
        job_list = sorted((d for d in root.iterdir() if d.is_dir()), key=lambda d: d.stat().st_mtime, reverse=True)
    except OSError:
        job_list = []
    for job_dir in job_list:
        # Harbor writes the job's own result.json when the whole job ends.
        job_running = not (job_dir / "result.json").is_file()
        trials = [read_trial(d, job_running=job_running) for d in trial_dirs(job_dir)]
        if not trials:
            continue
        metas = [t.pop("_harbor") for t in trials]
        job_round = round_of(job_dir.name, rounds)
        declared = round_model.get(job_round)
        for t in trials:
            t["round"] = job_round
            t["round_model"] = declared
            # A model-calling trial that names no model yet (no config, no result) takes its round's.
            if t["model_id"] is None and t["kind"] != "oracle" and declared:
                t["model_id"] = declared
                t["provider"] = t["provider"] or model_provider(declared)
            # The round says one model, the trial ran another: never fold it into the round's numbers silently.
            t["model_mismatch"] = bool(t["model_id"] and declared and t["model_id"] != declared)
        scored = [t for t in trials if t["reward"] is not None]
        kinds = sorted({t["kind"] for t in trials})
        jobs.append(
            {
                "name": job_dir.name,
                "round": job_round,
                "started": min((t["started"] for t in trials if t["started"]), default=None),
                "kind": ", ".join(kinds),
                "model": ", ".join(sorted({t["model"] for t in trials if t["model"]})) or "—",
                "model_ids": sorted({t["model_id"] for t in trials if t["model_id"]}),
                "providers": sorted({t["provider"] for t in trials if t["provider"]}),
                "profile": ", ".join(sorted({t["profile"] for t in trials if t["profile"]})) or "—",
                "n_trials": len(trials),
                "passed": sum(1 for t in scored if t["reward"] >= 1.0),
                "scored": len(scored),
                "running": sum(1 for t in trials if t["status"] in ("running", "checking")),
                "errors": sum(1 for t in trials if t["status"] == "error"),
                "cost_usd": round(sum(t["cost_usd"] or 0 for t in trials), 2),
                "unpriced_runs": sum(1 for t in trials if t["cost_usd"] is None and (t["input_tokens"] or 0) > 0),
                "harbor": harbor_job(job_dir, trials, metas, hub),
                "trials": trials,
            }
        )
    return jobs


def model_order(rounds, trials):
    """Every model id seen, in a stable order: rounds.json order first, then
    models that only appear in trials. The "scripted" provider is the harness's
    own test double for plumbing checks, not a model. Returns [(id, has_rounds)]."""
    out = []
    for r in rounds:
        m = r.get("model")
        if m and m not in [x[0] for x in out]:
            out.append((m, True))
    for m in sorted({t["model_id"] for t in trials if t.get("model_id") and t.get("provider") != "scripted"}):
        if m not in [x[0] for x in out]:
            out.append((m, False))
    return out


def _task_slot():
    return {"agent_attempts": 0, "agent_passed": 0, "terminus_attempts": 0, "terminus_passed": 0, "last": None}


def task_rows(jobs, catalog, split):
    """One row per task. The agent_*/terminus_* totals span every model; per_model
    holds the same counts per model id, so accuracy is never averaged across models."""
    rows = {name: dict(meta, agent_attempts=0, agent_passed=0, oracle_attempts=0, oracle_passed=0, terminus_attempts=0, terminus_passed=0, last=None, per_model={}) for name, meta in catalog.items()}
    for job in jobs:
        for t in job["trials"]:
            row = rows.setdefault(t["task"], {"task": t["task"], "difficulty": "?", "category": "?", "tags": [], "agent_attempts": 0, "agent_passed": 0, "oracle_attempts": 0, "oracle_passed": 0, "terminus_attempts": 0, "terminus_passed": 0, "last": None, "per_model": {}})
            if t["reward"] is None:
                continue
            prefix = t["kind"] if t["kind"] in ("oracle", "terminus") else "agent"
            last = {"job": job["name"], "trial": t["trial"], "status": t["status"], "started": t["started"]}
            targets = [row]
            if prefix != "oracle":
                targets.append(row["per_model"].setdefault(t["model_id"] or "unknown", _task_slot()))
            for tgt in targets:
                tgt[prefix + "_attempts"] += 1
                tgt[prefix + "_passed"] += 1 if t["reward"] >= 1.0 else 0
                if prefix == "agent" and (tgt["last"] is None or (t["started"] or "") > (tgt["last"]["started"] or "")):
                    tgt["last"] = last
    dev = set(split.get("dev", [])) if split else set()
    heldout = set(split.get("heldout", [])) | set(split.get("held_out", [])) if split else set()
    for name, row in rows.items():
        row["split"] = "dev" if name in dev else "held-out" if name in heldout else ("—" if split else "no split yet")
        row["seen"] = row["agent_attempts"] > 0
        # The task's own reference solution never passed and no agent ever
        # passed it either: this machine cannot run the task. (When an agent
        # passes, the reference solution is what is broken, not the machine.)
        row["oracle_broken"] = row["oracle_attempts"] > 0 and row["oracle_passed"] == 0
        row["machine_blocked"] = row["oracle_broken"] and row["agent_passed"] == 0 and row["terminus_passed"] == 0
    return sorted(rows.values(), key=lambda r: r["task"])


COMPARED = ("gitagent", "terminus")


def _median(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    m = len(xs) // 2
    return xs[m] if len(xs) % 2 else (xs[m - 1] + xs[m]) / 2


def _mean(xs):
    xs = [x for x in xs if x is not None]
    return sum(xs) / len(xs) if xs else None


def agent_stats(trials):
    """What one agent did over a set of trials (scored runs only for rates)."""
    scored = [t for t in trials if t["reward"] is not None]
    timed_out = [t for t in trials if t["exception"] == "AgentTimeoutError" or (t["outcome"] or "").startswith("time_up")]
    finished = [t for t in trials if t["status"] in ("pass", "fail", "error", "no_verdict")]
    inp = sum(t["input_tokens"] or 0 for t in trials)
    cache = sum(t["cache_tokens"] or 0 for t in trials)
    cost = sum(t["cost_usd"] or 0 for t in trials)
    passed = sum(1 for t in scored if t["reward"] >= 1.0)
    return {
        "runs": len(trials),
        "scored": len(scored),
        "passed": passed,
        "pass_rate": passed / len(scored) if scored else None,
        "timeouts": len(timed_out),
        "timeout_rate": len(timed_out) / len(finished) if finished else None,
        "median_agent_s": _median(t["agent_s"] for t in scored),
        "mean_turns": _mean(t["turns"] for t in scored),
        "input_tokens": inp,
        "cache_tokens": cache,
        "output_tokens": sum(t["output_tokens"] or 0 for t in trials),
        "cache_share": cache / inp if inp else None,
        "cost_usd": round(cost, 4),
        "cost_per_run": cost / len(scored) if scored else None,
        "cost_per_pass": cost / passed if passed else None,
        "tokens_per_run": (inp + sum(t["output_tokens"] or 0 for t in trials)) / len(scored) if scored else None,
        # Runs that used tokens on a model bench/pricing.json does not price: their cost is unknown, not 0.
        "unpriced_runs": sum(1 for t in trials if t["cost_usd"] is None and (t["input_tokens"] or 0) > 0),
    }


def comparison(jobs, task_meta, model=None):
    """gitagent against Terminus, per task and overall, from scored runs of one
    model (both sides always the same model; None only when nothing names one)."""
    by_task = {}
    for job in jobs:
        for t in job["trials"]:
            if t["kind"] in COMPARED and t.get("model_id") == model:
                by_task.setdefault(t["task"], {k: [] for k in COMPARED})[t["kind"]].append(t)
    rows = []
    for task, runs in sorted(by_task.items()):
        meta = task_meta.get(task, {})
        row = {"task": task, "difficulty": meta.get("difficulty", "?"), "split": meta.get("split", "?"),
               "machine_blocked": meta.get("machine_blocked", False)}
        for k in COMPARED:
            st = agent_stats(runs[k])
            row[k] = {key: st[key] for key in ("runs", "scored", "passed", "pass_rate", "median_agent_s", "mean_turns", "cost_usd")}
            n = max(st["scored"], 1)
            row[k]["input_tokens"] = st["input_tokens"] / n if st["scored"] else None
            row[k]["cache_tokens"] = st["cache_tokens"] / n if st["scored"] else None
            row[k]["output_tokens"] = st["output_tokens"] / n if st["scored"] else None
            row[k]["cost_per_run"] = st["cost_per_run"]
        rows.append(row)
    usable = [r for r in rows if not r["machine_blocked"]]
    paired = [r for r in usable if r["gitagent"]["scored"] and r["terminus"]["scored"]]

    def overall(kind, subset):
        trials = [t for r in subset for t in by_task[r["task"]][kind]]
        st = agent_stats(trials)
        # The blueprint's pass rate: each task's mean reward, averaged over tasks.
        per_task = [r[kind]["pass_rate"] for r in subset if r[kind]["pass_rate"] is not None]
        st["task_pass_rate"] = sum(per_task) / len(per_task) if per_task else None
        st["tasks"] = len(per_task)
        return st

    return {
        "model": model,
        "rows": rows,
        "paired_tasks": len(paired),
        "all": {k: overall(k, usable) for k in COMPARED},
        "paired": {k: overall(k, paired) for k in COMPARED},
        "wins": sum(1 for r in paired if (r["gitagent"]["pass_rate"] or 0) > (r["terminus"]["pass_rate"] or 0)),
        "losses": sum(1 for r in paired if (r["gitagent"]["pass_rate"] or 0) < (r["terminus"]["pass_rate"] or 0)),
    }


def round_split(r):
    """A round's task set: "dev", "held-out", or "all" (every task, the leaderboard format)."""
    return {"heldout": "held-out", "all": "all"}.get(r.get("split"), "dev")


def progress_of(kind, trials, meta, skip=(), split="dev"):
    """One harness over a set of trials: the split's tasks done, passed, running
    and still to run, and accuracy. The oracle is counted over every task, since
    it does not learn from them."""
    in_split = lambda t: t["split"] in ("dev", "held-out") if split == "all" else t["split"] == split  # noqa: E731
    scope = sorted(n for n, t in meta.items() if (kind == "oracle" or in_split(t)) and n not in skip)
    mine = [t for t in trials if t["kind"] == kind and t["task"] in scope]
    scored, live = {}, set()
    for t in mine:
        if t["reward"] is not None:
            scored.setdefault(t["task"], []).append(t["reward"])
        elif t["status"] in ("running", "checking"):
            live.add(t["task"])
    rates = {n: sum(1 for r in v if r >= 1.0) / len(v) for n, v in scored.items()}
    # Single-run accuracy: each task's k-th scored attempt, by start time.
    attempts = {}
    for t in sorted(mine, key=lambda t: t["started"] or ""):
        if t["reward"] is not None:
            attempts.setdefault(t["task"], []).append(1 if t["reward"] >= 1.0 else 0)
    depth = max((len(v) for v in attempts.values()), default=0)
    per_run = []
    for k in range(depth):
        got = [v[k] for v in attempts.values() if len(v) > k]
        per_run.append({"run": k + 1, "tasks": len(got), "passed": sum(got), "accuracy": sum(got) / len(got) if got else None})
    runnable = {n: r for n, r in rates.items() if not meta[n]["machine_blocked"]}
    stats = agent_stats(mine)
    starts = sorted(t["started"] for t in mine if t["started"])
    return {
        "scope": len(scope),
        "scope_label": "all tasks" if kind == "oracle" or split == "all" else f"{split} tasks",
        "per_run": per_run,
        "done": len(scored),
        "passed": sum(1 for r in rates.values() if r >= 1.0),
        "failed": sorted(n for n, r in rates.items() if r < 1.0),
        "running": sorted(live - set(scored)),
        "pending": [n for n in scope if n not in scored and n not in live],
        # The blueprint's pass rate: each task's mean reward, averaged over tasks.
        "accuracy": sum(rates.values()) / len(rates) if rates else None,
        "runnable_done": len(runnable),
        "runnable_accuracy": sum(runnable.values()) / len(runnable) if runnable else None,
        "runs": sum(len(v) for v in scored.values()),
        "skipped": sorted(skip),
        "cost_usd": stats["cost_usd"],
        "cost_per_run": stats["cost_per_run"],
        "unpriced_runs": stats["unpriced_runs"],
        "median_agent_s": stats["median_agent_s"],
        "timeouts": stats["timeouts"],
        "first": starts[0] if starts else None,
        "last": starts[-1] if starts else None,
    }


def primary_model(rounds, models):
    """The model with the most counting rounds (ties: first in rounds.json)."""
    best, best_n = None, -1
    for m, _ in models:
        n = sum(1 for r in rounds if r.get("counts", True) and r.get("model") == m)
        if n > best_n:
            best, best_n = m, n
    return best


def harness_progress(jobs, tasks, models):
    """The header's numbers, kept apart per model: each model's current gitagent
    round, its Terminus runs and its held-out run; the oracle (no model) once;
    plus one row per round, harness and model for the history table. The
    top-level gitagent/terminus/heldout keys are the primary model's."""
    meta = {t["task"]: t for t in tasks}
    rounds = load_rounds()
    by_name = {r["name"]: r for r in rounds}
    counted = [t for j in jobs for t in j["trials"] if by_name.get(t.get("round"), {}).get("counts", True)]
    primary = primary_model(rounds, models)

    def round_label(name):
        return by_name.get(name, {}).get("label", name)

    history = []
    for r in rounds:
        if not r.get("counts", True):
            continue
        in_round = [t for t in counted if t.get("round") == r["name"]]
        split = round_split(r)
        for kind in ("gitagent", "terminus", "oracle"):
            of_kind = [t for t in in_round if t["kind"] == kind]
            # One row per model that actually ran (the oracle calls no model).
            seen = sorted({t.get("model_id") for t in of_kind}, key=lambda m: (m is None, m or ""))
            for m in seen:
                mine = [t for t in of_kind if t.get("model_id") == m]
                row = progress_of(kind, mine, meta, r.get("skip", ()), split)
                row.update(kind=kind, model=m, round=r["name"], round_model=r.get("model"),
                           model_mismatch=bool(m and r.get("model") and m != r.get("model")),
                           label=r.get("label", r["name"]), host=r.get("host"), commit=r.get("commit"),
                           split=split, current=False)
                history.append(row)

    by_model = {}
    for m, has_rounds in models:
        mine = [t for t in counted if t.get("model_id") == m]
        with_git = [r for r in rounds if r.get("counts", True) and r.get("split", "dev") == "dev" and r.get("model") == m
                    and any(t["kind"] == "gitagent" and t.get("round") == r["name"] for t in mine)]
        current = with_git[-1] if with_git else None
        git_trials = [t for t in mine if t.get("round") == current["name"]] if current else []
        term_round = next((r for r in rounds if any(t["kind"] == "terminus" and t.get("round") == r["name"] for t in mine)), {})
        git = progress_of("gitagent", git_trials, meta, (current or {}).get("skip", ()))
        git["round"] = current
        held = [h for h in history if h["kind"] == "gitagent" and h["model"] == m and h["split"] == "held-out"]
        # The headline is the round marked "final" in rounds.json (the frozen,
        # multi-run score); a later single-run check never replaces it.
        final_names = {r["name"] for r in rounds if r.get("final") and r.get("model") == m}
        finals = [h for h in held if h["round"] in final_names]
        if finals:
            held = finals
        if current:
            for h in history:
                if h["round"] == current["name"] and h["model"] == m and h["kind"] == "gitagent":
                    h["current"] = True
        unrounded = [t for t in mine if t.get("round") is None]
        by_model[m] = {
            "model": m,
            "has_rounds": has_rounds,
            "rounds": [r["name"] for r in rounds if r.get("counts", True) and r.get("model") == m],
            # Trials in jobs that no round in rounds.json matches yet (a round that has started but is not listed).
            "unrounded_trials": len(unrounded),
            "unrounded_jobs": sorted({j["name"] for j in jobs if any(t.get("model_id") == m and t.get("round") is None for t in j["trials"])}),
            "gitagent": git,
            "terminus": progress_of("terminus", mine, meta, term_round.get("skip", ())),
            "terminus_round": term_round.get("name"),
            "heldout": dict(held[-1], label=round_label(held[-1]["round"])) if held else None,
            # The latest run over all tasks (the leaderboard format), if any.
            "full": next((dict(h, label=round_label(h["round"])) for h in reversed(history)
                          if h["kind"] == "gitagent" and h["model"] == m and h["split"] == "all"), None),
        }

    out = {
        "models": [m for m, _ in models],
        "primary_model": primary,
        "by_model": by_model,
        "oracle": progress_of("oracle", counted, meta),
        "history": history,
    }
    if primary:
        out["gitagent"] = by_model[primary]["gitagent"]
        out["terminus"] = by_model[primary]["terminus"]
        out["heldout"] = by_model[primary]["heldout"]
    else:
        out["gitagent"] = dict(progress_of("gitagent", [], meta), round=None)
        out["terminus"] = progress_of("terminus", [], meta)
        out["heldout"] = None
    return out


_lessons_cache = {}


def lessons_at(commit):
    """The lesson bullets in prompts/lessons.md at a commit, or None."""
    if not commit:
        return None
    if commit not in _lessons_cache:
        try:
            out = subprocess.run(["git", "-C", str(REPO), "show", f"{commit}:prompts/lessons.md"],
                                 capture_output=True, text=True, timeout=5)
            text = out.stdout if out.returncode == 0 else None
        except (OSError, subprocess.SubprocessError):
            text = None
        _lessons_cache[commit] = [l[2:].strip() for l in text.splitlines() if l.startswith("- ")] if text else None
    return _lessons_cache[commit]


def rounds_page(progress, jobs=()):
    """Each round with what it did, why it scored as it did, its accuracy per
    harness, the lessons its binary carried compared with the previous round's,
    and how its Harbor jobs were run (the compliance strip)."""
    out, prev = [], None
    for r in load_rounds():
        if not r.get("counts", True):
            continue
        rows = [h for h in progress["history"] if h["round"] == r["name"]]
        lessons = lessons_at(r.get("lessons_commit"))
        diff = None
        if lessons is not None and prev is not None:
            diff = {"added": [l for l in lessons if l not in prev], "dropped": [l for l in prev if l not in lessons]}
        out.append({k: r.get(k) for k in ("name", "label", "host", "commit", "changes", "reasoning", "lessons_commit", "model")}
                   | {"split": round_split(r),
                      "results": rows, "lessons": lessons, "lessons_diff": diff,
                      "compliance": round_compliance([j for j in jobs if j["round"] == r["name"]])})
        if lessons is not None:
            prev = lessons
    return out


def profiles():
    out = []
    for path in sorted(list((REPO / "profiles").glob("*.yaml")) + list((REPO / "bench" / "profiles").glob("*.yaml"))):
        text = read_text(path) or ""
        out.append({"name": str(path.relative_to(REPO)), "text": text, "flags": yaml_flags(text)})
    return out


def token_totals(agent_rows):
    """Token, cost and run counts over model-calling trials (one model, or all)."""
    def acc(kind):
        rows = [t for t in agent_rows if t["reward"] is not None and (t["kind"] == kind if kind else t["kind"] not in ("oracle", "terminus"))]
        return {"scored": len(rows), "passed": sum(1 for t in rows if t["reward"] >= 1.0)}

    live = [t for t in agent_rows if t["status"] in ("running", "checking")]
    return {
        "trials": len(agent_rows),
        "agent": acc(None),
        "terminus": acc("terminus"),
        "running": len(live),
        "input_tokens": sum(t["input_tokens"] or 0 for t in agent_rows),
        "output_tokens": sum(t["output_tokens"] or 0 for t in agent_rows),
        "cache_tokens": sum(t["cache_tokens"] or 0 for t in agent_rows),
        "cache_write_tokens": sum((t["counters"] or {}).get("cache_write_tokens", 0) for t in agent_rows),
        "cost_usd": round(sum(t["cost_usd"] or 0 for t in agent_rows), 2),
        "cost_by_kind": {
            k: round(sum(t["cost_usd"] or 0 for t in agent_rows if t["kind"] == k), 2)
            for k in sorted({t["kind"] for t in agent_rows if t["cost_usd"]})
        },
        "live_cost_usd": round(sum(t["cost_usd"] or 0 for t in live), 2),
        "unpriced_runs": sum(1 for t in agent_rows if t["cost_usd"] is None and (t["input_tokens"] or 0) > 0),
    }


def dev_accuracy(dev, slot_of):
    """The overview's dev numbers for one view of the task rows: slot_of(task row)
    gives the counts to read (the row itself, or one model's per_model entry)."""
    attempted = [(t, slot_of(t)) for t in dev if (slot_of(t) or {}).get("agent_attempts")]
    latest_pass = [t for t, s in attempted if (s["last"] or {}).get("status") == "pass"]
    by_diff = {}
    for t, s in attempted:
        b = by_diff.setdefault(t["difficulty"], {"attempted": 0, "passed": 0})
        b["attempted"] += 1
        b["passed"] += 1 if (s["last"] or {}).get("status") == "pass" else 0
    blocked = sorted(t["task"] for t, _ in attempted if t["machine_blocked"])
    runnable = [(t, s) for t, s in attempted if not t["machine_blocked"]]
    return {
        "dev_runnable": len(runnable),
        "dev_runnable_passed": sum(1 for _, s in runnable if (s["last"] or {}).get("status") == "pass"),
        "machine_blocked": blocked,
        "dev_total": len(dev),
        "dev_attempted": len(attempted),
        "dev_latest_passed": len(latest_pass),
        "dev_any_passed": sum(1 for _, s in attempted if s["agent_passed"]),
        "by_difficulty": dict(sorted(by_diff.items())),
    }


def summary(root: Path):
    jobs = scan(root)
    all_trials = [t for j in jobs for t in j["trials"]]
    agent_rows = [t for t in all_trials if t["kind"] != "oracle"]
    rounds = load_rounds()
    models = model_order(rounds, agent_rows)
    model_ids = [m for m, _ in models]
    oracle_scored = [t for t in all_trials if t["kind"] == "oracle" and t["reward"] is not None]
    totals = dict(token_totals(agent_rows))
    totals.update({
        "jobs": len(jobs),
        "trials": len(all_trials),
        "oracle": {"scored": len(oracle_scored), "passed": sum(1 for t in oracle_scored if t["reward"] >= 1.0)},
        "running": sum(1 for t in all_trials if t["status"] in ("running", "checking")),
        "runaway": [
            {"job": j["name"], "trial": t["trial"], "task": t["task"], "kind": t["kind"], "model_id": t["model_id"], "cost_usd": t["cost_usd"]}
            for j in jobs for t in j["trials"]
            if t["status"] in ("running", "checking") and (t["cost_usd"] or 0) >= RUNAWAY_USD
        ],
        "runaway_usd": RUNAWAY_USD,
        "priced_models": sorted(pricing()),
        # Models that ran and used tokens but have no price: their spend shows as n/a, never 0.
        "unpriced_models": sorted({t["model_id"] or "unknown" for t in agent_rows
                                   if t["cost_usd"] is None and (t["input_tokens"] or 0) > 0 and t["provider"] != "scripted"}),
        # Everything above, per model, so no tile or card mixes two models' tokens or money.
        "by_model": {m: token_totals([t for t in agent_rows if t["model_id"] == m]) for m in model_ids},
    })
    guard_totals = {}
    for t in agent_rows:
        for k, v in (t["counters"] or {}).items():
            if isinstance(v, (int, float)):
                guard_totals[k] = guard_totals.get(k, 0) + v
            elif isinstance(v, dict):
                for kk, vv in v.items():
                    guard_totals[f"{k}.{kk}"] = guard_totals.get(f"{k}.{kk}", 0) + vv

    split = load_json(REPO / "bench" / "split.json")
    tasks = task_rows(jobs, task_catalog(), split)
    dev = [t for t in tasks if t["split"] == "dev"]
    accuracy = dev_accuracy(dev, lambda t: t)
    blocked = accuracy["machine_blocked"]
    # Explain machine failures on the trials themselves too.
    for job in jobs:
        for tr in job["trials"]:
            if tr["kind"] != "oracle" and tr["task"] in blocked and tr["status"] in ("fail", "error"):
                tr["machine_blocked"] = True
                tr["diagnosis"]["notes"].insert(0, "The task's own reference solution also fails on this machine (oracle run), so this failure is the machine's, not the agent's.")
    accuracy.update({
        "heldout_total": sum(1 for t in tasks if t["split"] == "held-out"),
        "heldout_runs": sum(t["agent_attempts"] for t in tasks if t["split"] == "held-out"),
        "has_split": split is not None,
        # The same dev numbers per model: the cards never average two models together.
        "by_model": {
            m: dict(dev_accuracy(dev, lambda t, m=m: t["per_model"].get(m)),
                    heldout_runs=sum((t["per_model"].get(m) or {}).get("agent_attempts", 0) for t in tasks if t["split"] == "held-out"))
            for m in model_ids
        },
    })
    progress = harness_progress(jobs, tasks, models)
    task_meta = {t["task"]: t for t in tasks}
    by_model_cmp = {}
    for m in model_ids:
        cur = ((progress["by_model"][m]["gitagent"].get("round") or {})).get("name")
        cmp = comparison(
            [dict(j, trials=[t for t in j["trials"] if t["kind"] != "gitagent" or (cur and t.get("round") == cur)]) for j in jobs],
            task_meta, m)
        cmp["gitagent_round"] = progress["by_model"][m]["gitagent"].get("round")
        by_model_cmp[m] = cmp
    primary = progress["primary_model"]
    compare = dict(by_model_cmp[primary]) if primary else dict(comparison([], task_meta), gitagent_round=None)
    compare.update(models=model_ids, default_model=primary, by_model=by_model_cmp)
    lessons = load_json(REPO / "bench" / "lessons" / "lessons.json")
    rounds_view = rounds_page(progress, jobs)
    for job in jobs:
        job["harbor"].pop("_versions", None)
    return {
        "models": {m: model_info(m, i, has_rounds) for i, (m, has_rounds) in enumerate(models)},
        "compare": compare,
        "progress": progress,
        "rounds_page": rounds_view,
        "leaderboard": load_json(REPO / "bench" / "leaderboard.json") or {},
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "root": str(root),
        "totals": totals,
        "guard_totals": guard_totals,
        "jobs": jobs,
        "tasks": tasks,
        "accuracy": accuracy,
        "split": split,
        "lessons": lessons,
        "lessons_md": read_text(REPO / "prompts" / "lessons.md"),
        "learner_md": read_text(REPO / "prompts" / "learner.md"),
        "profiles": profiles(),
        "docs": {
            "INTEGRITY.md": read_text(REPO / "docs" / "INTEGRITY.md"),
            "RESULTS.md": read_text(REPO / "bench" / "RESULTS.md"),
        },
    }


# ---------------------------------------------------------------- trial detail

def classify_note(text: str):
    t = text.lstrip()
    if t.startswith("You have repeated the same action"):
        return "loop"
    if t.startswith("Time check:"):
        return "time_nudge"
    if t.startswith("Time is almost up"):
        return "force_stop"
    if t.startswith("Every response must call a tool") or t.startswith("Your response was cut off"):
        return "no_tool_nudge"
    if "# Progress so far" in t:
        return "compaction"
    if t.startswith("# Next task"):
        return "next_task"
    return None


def classify_result(text: str):
    t = text.lstrip()
    if t.startswith("Not marked complete:"):
        return "stop_rejection"
    if t.startswith("Refused: nothing may be written under"):
        return "protected_write"
    if t.startswith("Blocked by a hook"):
        return "hook_block"
    if t.startswith("Sub-agent `"):
        return "subagent"
    return None


def safe_trial_dir(root: Path, job: str, trial: str):
    if not (SAFE_NAME.match(job or "") and SAFE_NAME.match(trial or "")):
        return None
    trial_dir = (root / job / trial).resolve()
    if not str(trial_dir).startswith(str(root.resolve()) + os.sep) or not trial_dir.is_dir():
        return None
    return trial_dir


def trial_detail(root: Path, job: str, trial: str):
    trial_dir = safe_trial_dir(root, job, trial)
    if trial_dir is None:
        return None
    result = load_json(trial_dir / "result.json") or {}
    row = read_trial(trial_dir)
    task_record = row.pop("_harbor", None) or {}
    detail = {"row": row, "timings": [], "steps": [], "events": [], "turns": [], "files": [], "log_tail": ""}

    for key in ("environment_setup", "agent_setup", "agent_execution", "verifier"):
        s = seg_seconds(result, key)
        if s is not None:
            detail["timings"].append({"phase": key, "seconds": s})

    traj = load_json(trial_dir / "agent" / "trajectory.json") or {}
    detail["outcome"] = (traj.get("extra") or {}).get("outcome")
    detail["elapsed_s"] = (traj.get("extra") or {}).get("elapsed_s")
    prev_ts = None
    for i, step in enumerate(traj.get("steps") or []):
        src, msg = step.get("source"), step.get("message") or ""
        extra = step.get("extra") or {}
        s = {
            "id": step.get("step_id"),
            "source": src,
            "timestamp": step.get("timestamp"),
            "message": cap(msg),
            "calls": [],
            "observations": [],
            "metrics": step.get("metrics"),
            "compaction": bool(extra.get("compaction")),
            "kind": None,
        }
        if src == "user" and i > 1:
            s["kind"] = classify_note(msg)
            if s["kind"]:
                detail["events"].append({"step": s["id"], "kind": s["kind"], "text": cap(msg, 300)})
        if s["compaction"]:
            detail["events"].append({"step": s["id"], "kind": "compaction_summary", "text": cap(msg, 300)})
        for call in step.get("tool_calls") or []:
            args = json.dumps(call.get("arguments") or {}, ensure_ascii=False)
            s["calls"].append({"name": call.get("function_name"), "arguments": cap(args, 4000)})
        for obs in (step.get("observation") or {}).get("results") or []:
            content = obs.get("content") or ""
            if content:
                s["observations"].append(cap(content))
                kind = classify_result(content)
                if kind:
                    detail["events"].append({"step": s["id"], "kind": kind, "text": cap(content, 300)})
        if src == "agent":
            ts = parse_iso(step.get("timestamp"))
            detail["turns"].append(
                {
                    "step": s["id"],
                    "tools": [c["name"] for c in s["calls"]],
                    "prompt_tokens": (s["metrics"] or {}).get("prompt_tokens"),
                    "completion_tokens": (s["metrics"] or {}).get("completion_tokens"),
                    "since_prev_s": round((ts - prev_ts).total_seconds(), 1) if ts and prev_ts else None,
                }
            )
            if ts:
                prev_ts = ts
        detail["steps"].append(s)

    counters = ((result.get("agent_result") or {}).get("metadata") or {}).get("counters")
    detail["counters"] = counters if counters is not None else load_json(trial_dir / "agent" / "counters.json")

    agent_dir = trial_dir / "agent"
    for sub in ("commands", "out", "setup"):
        base = agent_dir / sub
        if not base.is_dir():
            continue
        for p in sorted(base.rglob("*")):
            if p.is_file():
                rel = str(p.relative_to(agent_dir))
                if SAFE_REL.match(rel):
                    detail["files"].append({"path": rel, "bytes": p.stat().st_size})
    if (agent_dir / "gitagent.log").is_file():
        detail["files"].append({"path": "gitagent.log", "bytes": (agent_dir / "gitagent.log").stat().st_size})

    log = read_text(agent_dir / "gitagent.log", 128 * 1024)
    if log:
        detail["log_tail"] = "\n".join(log.splitlines()[-LOG_TAIL_LINES:])
    exc = result.get("exception_info")
    detail["harbor"] = {
        "exception": exc if isinstance(exc, dict) else ({"exception_message": str(exc)} if exc else None),
        "done": (read_text(agent_dir / "done", 64) or "").strip(),
        "has_trajectory": (agent_dir / "trajectory.json").is_file(),
        "has_log": (agent_dir / "gitagent.log").is_file(),
    }
    # Task version and run settings from the trial's result.json (see trial_harbor).
    detail["harbor"].update(task_record)
    return detail


def trial_file(root: Path, job: str, trial: str, rel: str):
    trial_dir = safe_trial_dir(root, job, trial)
    if trial_dir is None or not SAFE_REL.match(rel or ""):
        return None
    path = (trial_dir / "agent" / rel).resolve()
    if not str(path).startswith(str(trial_dir / "agent") + os.sep) or not path.is_file():
        return None
    return read_text(path)


# ---------------------------------------------------------------- page

PAGE = r"""<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>gitagent bench</title>
<style>
/* ---- tokens: one palette, one spacing scale, one type scale ---- */
:root {
  color-scheme: light;
  --bg:#f3f5f8; --panel:#ffffff; --code:#f0f2f5; --ink:#1c2430; --ink-2:#3d4857; --muted:#67727f;
  --line:#e1e5eb; --line-strong:#c9d0d9;
  --accent:#2457a6; --accent-bg:#e6eefa;
  --pass:#1f7a3d; --pass-bg:#e2f3e7; --fail:#b3261e; --fail-bg:#fbe7e5; --warn:#8a5a00; --warn-bg:#fcf0d4;
  --run:#2457a6; --run-bg:#e6eefa; --neutral:#5a6573; --neutral-bg:#eaeef2;
  /* model chips: one quiet hue per model family, assigned by order of appearance */
  --m0:#3b4f6b; --m0-bg:#e7ecf3; --m1:#6b4f3b; --m1-bg:#f2ebe4; --m2:#3f5f52; --m2-bg:#e5efea;
  --sans:system-ui,-apple-system,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;
  --mono:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  --s1:4px; --s2:8px; --s3:12px; --s4:16px; --s5:24px;
  --r:8px; --r-s:5px;
  --shadow:0 1px 2px rgba(16,24,40,.04);
}
@media (prefers-color-scheme: dark) {
  :root { color-scheme: dark;
    --bg:#0f1419; --panel:#161c24; --code:#1d2530; --ink:#e6eaf0; --ink-2:#c3cad4; --muted:#929dab;
    --line:#273140; --line-strong:#36424f;
    --accent:#7fa8e8; --accent-bg:#1c2a40;
    --pass:#6cc580; --pass-bg:#15301d; --fail:#f08a80; --fail-bg:#3a1c19; --warn:#e2b454; --warn-bg:#352a10;
    --run:#7fa8e8; --run-bg:#1c2a40; --neutral:#a3adba; --neutral-bg:#242d38; --shadow:none;
    --m0:#aebdd3; --m0-bg:#1f2938; --m1:#d1b9a3; --m1-bg:#302722; --m2:#a9c6b8; --m2-bg:#1d2b25; }
}
* { box-sizing:border-box; }
html { -webkit-text-size-adjust:100%; }
body { margin:0; background:var(--bg); color:var(--ink); font:14px/1.5 var(--sans); }
.wrap { max-width:1360px; margin:0 auto; padding:var(--s4) var(--s4) 64px; display:grid; gap:var(--s4); min-width:0; }
h1 { font-size:18px; font-weight:650; margin:0; letter-spacing:-.01em; }
h2 { font-size:15px; font-weight:650; margin:0 0 var(--s2); letter-spacing:-.005em; }
h3 { font-size:13px; font-weight:650; margin:var(--s3) 0 var(--s1); }
p { margin:0 0 var(--s2); max-width:90ch; }
p.muted { font-size:13px; }
ul, ol { margin:var(--s1) 0 var(--s2) 20px; padding:0; } li { margin:2px 0; }
a { color:var(--accent); text-decoration:none; } a:hover { text-decoration:underline; }
code { font:12px var(--mono); background:var(--code); padding:0 4px; border-radius:3px; }
pre { background:var(--code); border-radius:6px; padding:10px 12px; margin:0; max-height:60vh; overflow:auto;
  font:12px/1.5 var(--mono); white-space:pre-wrap; overflow-wrap:anywhere; }
pre code { background:none; padding:0; }
.muted { color:var(--muted); }
.ok { color:var(--pass); } .bad { color:var(--fail); }
.num { font-variant-numeric:tabular-nums; white-space:nowrap; text-align:right; }
.dt { font-variant-numeric:tabular-nums; white-space:nowrap; }
.mono { font-family:var(--mono); font-size:12px; }

/* ---- header ---- */
.top { display:flex; flex-wrap:wrap; gap:var(--s2) var(--s4); align-items:baseline; justify-content:space-between; }
.top .brand { display:flex; flex-wrap:wrap; gap:var(--s1) var(--s3); align-items:baseline; }
.top .sub { color:var(--muted); font-size:12.5px; }
.kpis { display:grid; gap:var(--s3); grid-template-columns:repeat(auto-fit, minmax(230px, 1fr)); }
.chips { display:flex; flex-wrap:wrap; gap:var(--s2); }
.chip { display:inline-flex; flex-wrap:wrap; gap:0 5px; align-items:baseline; background:var(--panel); border:1px solid var(--line);
  border-radius:999px; padding:3px 12px; font-size:12.5px; color:var(--muted); line-height:1.5; }
.chip b { color:var(--ink); font-weight:600; font-variant-numeric:tabular-nums; }
.chip.chip-alarm { border-color:var(--fail); background:var(--fail-bg); color:var(--fail); }
.chip.chip-alarm b { color:var(--fail); }
.chips.strip { margin:0 0 var(--s3); }

/* ---- tabs ---- */
.tabs { position:sticky; top:0; z-index:20; display:flex; flex-wrap:wrap; gap:2px; background:var(--bg);
  border-bottom:1px solid var(--line); margin:0 calc(-1 * var(--s4)); padding:var(--s1) var(--s4) 0; }
.tab { background:none; border:0; border-bottom:2px solid transparent; margin-bottom:-1px; padding:9px 12px; font:inherit; font-size:13.5px;
  color:var(--muted); cursor:pointer; white-space:nowrap; border-radius:var(--r-s) var(--r-s) 0 0; }
.tab:hover { color:var(--ink); background:var(--code); }
.tab[aria-selected="true"] { color:var(--ink); border-bottom-color:var(--accent); font-weight:600; }
.tab .cnt { display:inline-block; font-size:11px; font-weight:600; background:var(--code); color:var(--muted); border-radius:999px; padding:0 7px; margin-left:6px; line-height:17px; }
.tab[aria-selected="true"] .cnt { background:var(--accent-bg); color:var(--accent); }
.view { display:grid; gap:var(--s4); min-width:0; } .view[hidden] { display:none; }

/* ---- panels, cards, stat tiles ---- */
.panel { background:var(--panel); border:1px solid var(--line); border-radius:10px; padding:var(--s4) 18px; min-width:0; box-shadow:var(--shadow); }
.panel > p.muted:last-of-type { margin-bottom:var(--s3); }
.cols { display:grid; gap:var(--s3); grid-template-columns:repeat(auto-fit, minmax(300px, 1fr)); min-width:0; }
.card { border:1px solid var(--line); border-radius:var(--r); padding:var(--s3) 14px; min-width:0; background:var(--panel); }
.card h3 { margin-top:0; }
.card p { font-size:13px; }
.cardlist { display:grid; gap:var(--s2); }
.stat { border:1px solid var(--line); border-radius:var(--r); padding:var(--s3) 14px; display:grid; gap:3px; align-content:start; min-width:0; background:var(--panel); }
.stat .big { font-size:26px; font-weight:650; font-variant-numeric:tabular-nums; line-height:1.15; letter-spacing:-.01em; overflow-wrap:anywhere; }
.stat .big .sm { font-size:14px; font-weight:500; color:var(--muted); }
.stat .lbl { font-size:13px; font-weight:600; color:var(--ink-2); display:flex; flex-wrap:wrap; gap:6px; align-items:center; }
.stat .exp { color:var(--muted); font-size:12.5px; line-height:1.45; }
.stat.final { border-left:3px solid var(--accent); }
.stat.alarm { border-left:3px solid var(--fail); }
.stat.kpi .lbl { order:-1; font-size:11.5px; font-weight:600; text-transform:uppercase; letter-spacing:.05em; color:var(--muted); }
.stat .lbl .badge { text-transform:none; letter-spacing:0; }
.stat.kpi .big { font-size:28px; }
.stat.click { cursor:pointer; } .stat.click:hover { border-color:var(--line-strong); background:var(--code); }
.empty { padding:18px var(--s4); text-align:center; color:var(--muted); font-size:13px; border:1px dashed var(--line-strong); border-radius:var(--r); }
.empty code { background:var(--panel); }

/* ---- badges (status, agent kind, difficulty) ---- */
.badge { display:inline-flex; align-items:center; gap:5px; border-radius:999px; padding:0 8px; font-size:11.5px; font-weight:600; line-height:18px;
  white-space:nowrap; color:var(--neutral); background:var(--neutral-bg); vertical-align:middle; }
.b-pass { color:var(--pass); background:var(--pass-bg); }
.b-fail, .b-error { color:var(--fail); background:var(--fail-bg); }
.b-running { color:var(--run); background:var(--run-bg); }
.b-checking, .b-warn { color:var(--warn); background:var(--warn-bg); }
.b-no_verdict, .b-abandoned { color:var(--neutral); background:var(--neutral-bg); }
.badge.pulse::before { content:""; width:6px; height:6px; border-radius:50%; background:currentColor; }
.k-gitagent { color:var(--accent); background:var(--accent-bg); }
.k-terminus { color:var(--warn); background:var(--warn-bg); }
.k-oracle, .k-unknown { color:var(--neutral); background:var(--neutral-bg); }
.k-easy { color:var(--pass); background:var(--pass-bg); } .k-medium { color:var(--warn); background:var(--warn-bg); } .k-hard { color:var(--fail); background:var(--fail-bg); }
.m-0 { color:var(--m0); background:var(--m0-bg); } .m-1 { color:var(--m1); background:var(--m1-bg); } .m-2 { color:var(--m2); background:var(--m2-bg); }
.grouphead { display:flex; flex-wrap:wrap; gap:var(--s2); align-items:center; margin-bottom:calc(-1 * var(--s2)); }
.grouphead h2 { margin:0; }
.grouphead + .panel { margin-top:0; }
.bymodel { display:grid; gap:var(--s3); }
.bymodel h3 { margin:0; display:flex; gap:6px; align-items:center; }
.filters .subtabs { margin:0; }
a .badge { cursor:pointer; } a:hover .badge { outline:2px solid var(--line-strong); }
.pulse { animation:pulse 1.2s infinite alternate; }
@keyframes pulse { from{opacity:1;} to{opacity:.5;} }
@media (prefers-reduced-motion: reduce){ .pulse{animation:none;} }

/* ---- tables ---- */
.scroll { overflow-x:auto; min-width:0; }
table { border-collapse:collapse; width:100%; font-size:13px; }
th, td { text-align:left; padding:7px 10px; border-bottom:1px solid var(--line); vertical-align:top; white-space:nowrap; }
th { color:var(--muted); font-weight:600; font-size:11.5px; text-transform:uppercase; letter-spacing:.04em; white-space:nowrap; border-bottom-color:var(--line-strong); }
thead tr + tr th { font-size:11px; }
tbody tr:last-child td { border-bottom:0; }
tbody tr:hover td { background:var(--code); }
tbody tr.sel td { background:var(--accent-bg); }
tbody tr.click { cursor:pointer; }
td.wrap, .wrap-text { white-space:normal; max-width:60ch; min-width:28ch; }
td.wrap-s { white-space:normal; max-width:52ch; min-width:32ch; }
td details > span, td details > div { display:block; max-width:44ch; white-space:normal; }
td .badge + b, td .badge + a { margin-left:2px; }
table.sortable th { cursor:pointer; user-select:none; }
table.sortable th:hover { color:var(--ink); }
th[aria-sort="ascending"]::after { content:" \2191"; color:var(--accent); }
th[aria-sort="descending"]::after { content:" \2193"; color:var(--accent); }
.bar { background:var(--code); border-radius:3px; height:6px; width:96px; display:inline-block; vertical-align:middle; overflow:hidden; }
.bar i { display:block; height:100%; background:var(--pass); }
.bar.b-acc i { background:var(--accent); }

/* ---- forms, filters, buttons ---- */
.filters { display:flex; flex-wrap:wrap; gap:var(--s2) var(--s3); align-items:center; margin-bottom:var(--s3); }
.filters label { display:inline-flex; align-items:center; gap:6px; font-size:13px; color:var(--muted); }
select, input[type=search] { font:inherit; font-size:13px; padding:4px 8px; border:1px solid var(--line-strong); border-radius:var(--r-s); background:var(--panel); color:var(--ink); }
input[type=search] { min-width:240px; }
input[type=checkbox] { accent-color:var(--accent); margin:0; }
.btn { background:var(--panel); border:1px solid var(--line-strong); border-radius:var(--r-s); color:var(--ink); cursor:pointer; padding:4px 10px; font:inherit; font-size:12.5px; }
.btn:hover { background:var(--code); }
.subtabs { display:flex; flex-wrap:wrap; gap:4px; margin:var(--s3) 0; }
.subtab { background:var(--panel); border:1px solid var(--line); border-radius:999px; padding:3px 11px; font:inherit; font-size:12.5px; cursor:pointer; color:var(--muted); }
.subtab:hover { color:var(--ink); border-color:var(--line-strong); }
.subtab[aria-selected="true"] { color:var(--accent); border-color:var(--accent); background:var(--accent-bg); font-weight:600; }
details { margin-top:var(--s1); } summary { cursor:pointer; color:var(--muted); font-size:12.5px; } summary:hover { color:var(--ink); }
details > pre, details > div, details > ol { margin-top:var(--s1); }

/* ---- detail pages (trial, round) ---- */
.pagehead { display:flex; flex-wrap:wrap; gap:var(--s2) var(--s3); align-items:center; margin:0 0 var(--s3); padding-bottom:var(--s3); border-bottom:1px solid var(--line); }
.pagehead .crumbs { color:var(--muted); font-size:12.5px; overflow-wrap:anywhere; }
.spacer { flex:1; }
.titlerow { display:flex; flex-wrap:wrap; gap:var(--s2); align-items:center; margin-bottom:var(--s3); }
.titlerow h2 { margin:0; font-size:18px; }
.kv { display:grid; grid-template-columns:max-content minmax(0,1fr); gap:4px 14px; font-size:13px; align-content:start; }
.kv .k { color:var(--muted); white-space:nowrap; }
.kv > span:not(.k) { overflow-wrap:anywhere; }
.reason { font-size:13px; }
.reason ul { margin:4px 0 0 18px; font-size:12.5px; }
.step { border:1px solid var(--line); border-radius:var(--r); padding:10px 12px; margin-bottom:var(--s2); min-width:0; }
.step.note { border-color:var(--warn); background:var(--warn-bg); }
.step .meta { display:flex; flex-wrap:wrap; gap:0 8px; align-items:baseline; font-size:12px; }
.step pre { margin-top:6px; }
.src { font-weight:700; font-size:11.5px; text-transform:uppercase; letter-spacing:.05em; }
.src-agent{color:var(--accent);} .src-user{color:var(--run);} .src-system{color:var(--muted);}
.call { font-family:var(--mono); font-size:12px; margin-top:6px; overflow-wrap:anywhere; }
.imp { font-family:var(--mono); font-size:12px; color:var(--warn); white-space:nowrap; }
.ev { display:grid; grid-template-columns:max-content max-content minmax(0,1fr); gap:4px 12px; padding:6px 0; border-bottom:1px solid var(--line); font-size:13px; }
.ev:last-child { border-bottom:0; }
.ev .tag { font-family:var(--mono); font-size:11.5px; color:var(--warn); }
.ev > span { overflow-wrap:anywhere; }
.lesson { display:grid; grid-template-columns:max-content minmax(0,1fr); gap:4px 12px; padding:8px 0; border-bottom:1px solid var(--line); font-size:13px; }
.lesson:last-child { border-bottom:0; }
.lesson .note { font-size:12px; color:var(--muted); }
.live { display:grid; gap:var(--s2); }
.live .row { display:grid; grid-template-columns:minmax(160px,auto) minmax(0,1fr) auto; gap:4px 14px; align-items:start; font-size:13px;
  padding:8px 10px; border:1px solid var(--line); border-radius:var(--r-s); }
.live .row > * { min-width:0; overflow-wrap:anywhere; }
.live .row .cmd { font-family:var(--mono); font-size:12px; color:var(--muted); }
@media (max-width: 760px) { .live .row { grid-template-columns:1fr; } }
.failure .head { display:flex; flex-wrap:wrap; gap:6px 8px; align-items:center; }
.failure p { margin:6px 0 0; font-size:13px; }
.failure pre { margin-top:6px; max-height:8em; }
.failure ul { margin:4px 0 0 18px; font-size:12.5px; }

/* ---- design tab diagrams ---- */
.flow { min-width:760px; display:block; }
.flow .node rect { fill:var(--panel); stroke:var(--line-strong); stroke-width:1.5; }
.flow .node text { fill:var(--ink); font-size:13px; font-weight:600; text-anchor:middle; }
.flow .node text.sub { fill:var(--muted); font-size:11px; font-weight:400; }
.flow .lbl { fill:var(--muted); font-size:11.5px; text-anchor:middle; }
.flow .edge { fill:none; stroke:var(--muted); stroke-width:1.6; stroke-dasharray:6 5; animation:march 1.1s linear infinite; }
.flow .edge.back { stroke:var(--accent); }
.flow .fa { fill:var(--muted); }
.flow .node.n1 rect { animation:glow 7s ease-in-out infinite 0s; }
.flow .node.n2 rect { animation:glow 7s ease-in-out infinite 1s; }
.flow .node.n3 rect { animation:glow 7s ease-in-out infinite 2s; }
.flow .node.n4 rect { animation:glow 7s ease-in-out infinite 3s; }
.flow .node.n5 rect { animation:glow 7s ease-in-out infinite 4s; }
.flow .node.n6 rect { animation:glow 7s ease-in-out infinite 5s; }
@keyframes march { to { stroke-dashoffset:-22; } }
@keyframes glow { 0%,20%,100% { fill:var(--panel); stroke:var(--line-strong); } 7% { fill:var(--accent-bg); stroke:var(--accent); } }
@media (prefers-reduced-motion: reduce) { .flow .edge, .flow .node rect { animation:none; } }

@media (max-width: 640px) {
  .wrap { padding:var(--s3) var(--s3) 48px; }
  .tabs { margin:0 calc(-1 * var(--s3)); padding:var(--s1) var(--s3) 0; }
  .panel { padding:var(--s3) 14px; }
  .stat.kpi .big { font-size:24px; }
}
</style>
<div class="wrap">
  <header class="top">
    <div class="brand"><h1>gitagent bench</h1><span class="sub">Terminal-Bench 2.1 · local runs, read live from <code>bench/jobs</code></span></div>
    <span class="sub" id="meta">loading…</span>
  </header>
  <div class="kpis" id="kpis"></div>
  <div class="chips" id="chips"></div>
  <nav class="tabs" id="tabs" role="tablist"></nav>

  <section class="view" id="v-overview">
    <div class="filters" id="model-filter"></div>
    <div class="panel"><h2>Progress by harness</h2>
      <p class="muted">One row per harness and model; numbers of different models are never combined. Each task counted once: its pass rate over all scored runs, averaged over tasks (the blueprint's measure). gitagent and Terminus 2 cover the dev split; the oracle covers every task. "Runnable" leaves out tasks whose own reference solution fails on this machine. Click a row to open its round.</p>
      <div class="scroll"><table id="progress"></table></div></div>
    <div class="panel"><h2>History by round</h2>
      <p class="muted">Every round of runs, oldest first, from <code>bench/rounds.json</code>, with the model it ran. The header and the gitagent rows above show each model's current round only; earlier rounds stay here for comparison. Rounds on the Mac ran amd64 under emulation, so their failures mix the machine's with the agent's. Click a column heading to sort; the model filter above narrows the list.</p>
      <div class="scroll"><table id="history" class="sortable"></table></div></div>
    <div class="panel"><h2>Accuracy</h2><p class="muted">Per model; a card never averages two models together.</p><div class="bymodel" id="accuracy"></div></div>
    <div class="panel"><h2>Tokens and cost (all model-calling runs)</h2><p class="muted">Money adds up across models; tokens, cache shares and per-run costs are shown per model because prices and caching differ between providers.</p><div class="bymodel" id="cost"></div></div>
    <div class="panel" id="live-panel"><h2>Running now</h2><div class="live" id="live"></div></div>
    <div class="panel"><h2>Jobs</h2><p class="muted">One job is one <code>harbor run</code>. Oracle jobs run the tasks' reference solutions to check the machine, not the agent; terminus jobs are the baseline agent. "Harbor mean" is Harbor's own mean reward from the job's result.json; a warn badge marks a gap of more than 0.5 points from the accuracy beside it. Click a job to see its trials and Harbor's record of it; click a heading to sort.</p>
      <div class="scroll"><table id="jobs" class="sortable"><thead><tr><th>Job</th><th>Started</th><th>Agent</th><th>Model</th><th>Profile</th>
      <th>Trials</th><th>Pass</th><th>Accuracy</th><th></th><th>Harbor mean</th><th>Est. cost</th></tr></thead><tbody></tbody></table></div></div>
    <div class="panel"><h2>Accuracy by task, per harness and model</h2><p class="muted">One row per task, harness and model (oracle runs are the machine check, shown alongside). Rows of different models are never averaged.</p><div class="scroll"><table id="taskacc" class="sortable">
      <thead><tr><th>Task</th><th>Difficulty</th><th>Harness</th><th>Model</th><th>Runs</th><th>Passed</th><th>Accuracy</th><th></th><th>Oracle</th></tr></thead><tbody></tbody></table></div></div>
  </section>

  <section class="view" id="v-rounds" hidden>
    <div class="panel"><h2>Our rounds against the public leaderboards</h2>
      <p class="muted">Published Terminal-Bench 2.1 results (all 89 tasks) from <code>bench/leaderboard.json</code>, next to our rounds. Our gitagent rounds cover the dev tasks we tune on, so they read high until the held-out run; the Terminus row was measured here on the same model and lines up with the public Opus 5.5 + Terminus 2 figure, which shows this machine reproduces leaderboard conditions. Highlighted rows are ours; click one to open the round.</p>
      <div class="scroll"><table id="lb-table" class="sortable"></table></div></div>
    <div id="round-cards" class="view"></div>
  </section>

  <section class="view" id="v-compare" hidden>
    <div class="panel"><h2>Targets (from the blueprint)</h2>
      <div class="filters"><label>model <select id="c-model"></select></label><span class="muted" id="cmp-model"></span></div>
      <p class="muted">Measured on tasks both agents ran with the same model, excluding tasks this machine cannot run; a page never pairs runs of two different models. gitagent's side uses that model's current round only (<span id="cmp-round">—</span>), so both ran on the same machine. With one run per task the numbers move a lot; the blueprint scores with three or more.</p>
      <div class="cols" id="targets"></div></div>
    <div class="panel"><h2>Overall, every scored run</h2><div class="scroll"><table id="cmp-overall"></table></div></div>
    <div class="panel"><h2>Per task</h2>
      <div class="filters">
        <label><input type="checkbox" id="c-paired"> only tasks both agents ran</label>
        <label>sort <select id="c-sort"><option value="task">task</option><option value="tokens">gitagent tokens</option><option value="delta">token difference</option><option value="cost">gitagent cost</option><option value="result">result difference</option></select></label>
        <span class="muted" id="cmp-n"></span>
      </div>
      <p class="muted">Tokens and cost are averages per run. "in" counts every prompt token, cached ones included; the cache column is the part served from the cache. Costs are estimated from bench/pricing.json for every harness, so they compare like for like; a harness's own cost figure is used only for models that file does not price.</p>
      <div class="scroll"><table id="cmp-tasks"></table></div></div>
  </section>

  <section class="view" id="v-trials" hidden>
    <div id="job-harbor"></div>
    <div class="panel">
      <div class="filters">
        <label>job <select id="f-job"><option value="">all</option></select></label>
        <label>agent <select id="f-kind"><option value="">all</option><option>gitagent</option><option>oracle</option><option>terminus</option></select></label>
        <label>model <select id="f-model"><option value="">all</option></select></label>
        <label>verdict <select id="f-status"><option value="">all</option><option value="pass">pass</option><option value="fail">fail</option><option value="error">error</option><option value="running">running</option><option value="checking">checking</option><option value="no_verdict">no verdict</option><option value="abandoned">stale</option></select></label>
        <span class="muted" id="trials-n"></span>
      </div>
      <div class="scroll"><table id="trials" class="sortable"><thead><tr><th>Trial</th><th>Task</th><th>Agent</th><th>Model</th><th>Verdict</th><th>Outcome</th><th>Turns</th>
      <th>In tok</th><th>Cached</th><th>Out tok</th><th>Est. cost</th><th>Agent s</th><th>Check s</th><th>Started</th></tr></thead><tbody></tbody></table></div>
    </div>
  </section>

  <section class="view" id="v-trial" hidden><div id="detail"></div></section>
  <section class="view" id="v-round" hidden><div id="round-detail" class="view"></div></section>

  <section class="view" id="v-failures" hidden>
    <div class="panel"><h2>Why trials did not pass</h2>
      <p class="muted">The reason comes from the result and gitagent's own counters. Each card names the model that ran; the model filter on the overview applies here too (<span id="failures-n"></span>). Open a trial to read the last turns; for a rejected task_complete the "turns" view shows what the agent verified before it stopped.</p>
      <div id="failures" class="cardlist"></div></div>
  </section>

  <section class="view" id="v-guards" hidden>
    <div class="panel"><h2>Guards, loops and context: totals over agent runs</h2>
      <p class="muted">Every feature in gitagent counts what it does. A zero means the feature never fired (or is switched off in that profile). Hover a name for its meaning; the glossary below explains each one.</p>
      <div id="guard-totals" class="chips"></div></div>
    <div class="panel"><h2>Per trial</h2><div class="scroll"><table id="guard-table" class="sortable"></table></div></div>
    <div class="panel"><h2>Glossary</h2><div class="scroll"><table id="glossary"><thead><tr><th>Counter</th><th>Meaning</th><th>Switch</th></tr></thead><tbody></tbody></table></div></div>
    <div class="panel"><h2>How a run ends (outcome)</h2><div class="scroll"><table id="outcomes"><thead><tr><th>Outcome</th><th>Meaning</th></tr></thead><tbody></tbody></table></div></div>
  </section>

  <section class="view" id="v-learning" hidden>
    <div class="cols">
      <div class="panel"><h2>How self-learning works</h2><div id="learning-how"></div></div>
      <div class="panel"><h2>Status</h2><div class="kv" id="learning-status"></div></div>
    </div>
    <div class="panel"><h2>Lesson store (bench/lessons/lessons.json)</h2><div id="lessons" class="muted">none yet</div></div>
    <div class="cols">
      <div class="panel"><h2>What the agent currently sees (prompts/lessons.md)</h2><pre id="lessons-md"></pre></div>
      <div class="panel"><h2>Instructions to the learner model (prompts/learner.md)</h2><pre id="learner-md"></pre></div>
    </div>
  </section>

  <section class="view" id="v-tasks" hidden>
    <div class="panel">
      <div class="filters">
        <input type="search" id="t-search" placeholder="search task, tag, description">
        <label>difficulty <select id="t-diff"><option value="">all</option><option>easy</option><option>medium</option><option>hard</option></select></label>
        <label>category <select id="t-cat"><option value="">all</option></select></label>
        <label>split <select id="t-split"><option value="">all</option><option>dev</option><option>held-out</option></select></label>
        <label><input type="checkbox" id="t-seen"> only tasks we ran</label>
        <span class="muted" id="tasks-n"></span>
      </div>
      <div class="scroll"><table id="tasks" class="sortable"><thead><tr><th>Task</th><th>Difficulty</th><th>Category</th><th>Tags</th><th>Agent limit</th><th>Check limit</th><th>Agent runs</th><th>Oracle runs</th><th>Split</th><th>Last run</th></tr></thead><tbody></tbody></table></div>
    </div>
  </section>

  <section class="view" id="v-design" hidden>
    <div class="panel"><h2>How one task gets solved</h2>
      <p class="muted">The agent is a loop. It asks the model what to do, runs the command the model chose, trims the output and runs its safety checks, then asks again. It stops when the model says the task is done (and the stop check agrees), or when it runs out of turns or time. Only then does Harbor run the task's hidden checker.</p>
      <div class="scroll"><svg class="flow" viewBox="0 0 960 250" width="100%" role="img" aria-label="The agent loop: ask the model, run the command, trim and check, repeat; then the hidden checker decides.">
        <defs><marker id="fa" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0L10 5 0 10z" class="fa"/></marker></defs>
        <g class="node n1"><rect x="10" y="70" width="130" height="56" rx="9"/><text x="75" y="94">The task</text><text x="75" y="112" class="sub">the instruction text</text></g>
        <g class="node n2"><rect x="200" y="70" width="150" height="56" rx="9"/><text x="275" y="94">Ask the model</text><text x="275" y="112" class="sub">whole history is sent</text></g>
        <g class="node n3"><rect x="410" y="70" width="150" height="56" rx="9"/><text x="485" y="94">Run the command</text><text x="485" y="112" class="sub">in one lasting shell</text></g>
        <g class="node n4"><rect x="620" y="70" width="170" height="56" rx="9"/><text x="705" y="94">Trim and check</text><text x="705" y="112" class="sub">loops, time, record it</text></g>
        <g class="node n5"><rect x="820" y="20" width="130" height="56" rx="9"/><text x="885" y="44">Model says done</text><text x="885" y="62" class="sub">stop check agrees</text></g>
        <g class="node n6 end"><rect x="820" y="150" width="130" height="56" rx="9"/><text x="885" y="174">Hidden checker</text><text x="885" y="192" class="sub">pass or fail</text></g>
        <path class="edge" d="M140 98 H194" marker-end="url(#fa)"/>
        <path class="edge" d="M350 98 H404" marker-end="url(#fa)"/>
        <path class="edge" d="M560 98 H614" marker-end="url(#fa)"/>
        <path class="edge" d="M790 98 L814 56" marker-end="url(#fa)"/>
        <path class="edge back" d="M705 126 V190 H275 V132" marker-end="url(#fa)"/>
        <path class="edge" d="M885 76 V144" marker-end="url(#fa)"/>
        <text x="490" y="214" class="lbl">next turn: the result goes back to the model (until done, out of turns, or out of time)</text>
      </svg></div></div>

    <div class="panel"><h2>How a benchmark pass runs</h2>
      <p class="muted">One <code>harbor run</code> is a job. Harbor starts several task containers at once on the bench machine (the workers), puts gitagent inside each one, waits, then runs the hidden checker and writes a result. The dashboard reads those results; the learner reads our own records plus the pass/fail verdicts and feeds lessons back into the prompt for the next pass. No distributed system: it all runs on one machine.</p>
      <div class="scroll"><svg class="flow" viewBox="0 0 960 260" width="100%" role="img" aria-label="A benchmark pass: Harbor starts parallel task containers, each runs gitagent then the checker, results feed the dashboard and the learner, lessons go back into the prompt.">
        <g class="node n1"><rect x="10" y="100" width="120" height="56" rx="9"/><text x="70" y="124">harbor run</text><text x="70" y="142" class="sub">one job</text></g>
        <g class="node n2"><rect x="190" y="20" width="200" height="50" rx="9"/><text x="290" y="41">Container 1 (worker)</text><text x="290" y="59" class="sub">task + gitagent + checker</text></g>
        <g class="node n2"><rect x="190" y="103" width="200" height="50" rx="9"/><text x="290" y="124">Container 2 (worker)</text><text x="290" y="142" class="sub">task + gitagent + checker</text></g>
        <g class="node n2"><rect x="190" y="186" width="200" height="50" rx="9"/><text x="290" y="207">Container 3 (worker)</text><text x="290" y="225" class="sub">-n sets how many at once</text></g>
        <g class="node n3"><rect x="450" y="100" width="150" height="56" rx="9"/><text x="525" y="124">result.json</text><text x="525" y="142" class="sub">one per trial</text></g>
        <g class="node n4"><rect x="660" y="30" width="130" height="56" rx="9"/><text x="725" y="54">Dashboard</text><text x="725" y="72" class="sub">this page</text></g>
        <g class="node n4"><rect x="660" y="170" width="130" height="56" rx="9"/><text x="725" y="194">Learner</text><text x="725" y="212" class="sub">gitagent learn</text></g>
        <g class="node n5 end"><rect x="830" y="170" width="120" height="56" rx="9"/><text x="890" y="194">Lessons</text><text x="890" y="212" class="sub">into the prompt</text></g>
        <path class="edge" d="M130 128 L184 45" marker-end="url(#fa)"/>
        <path class="edge" d="M130 128 H184" marker-end="url(#fa)"/>
        <path class="edge" d="M130 128 L184 211" marker-end="url(#fa)"/>
        <path class="edge" d="M390 45 L444 128" marker-end="url(#fa)"/>
        <path class="edge" d="M390 128 H444" marker-end="url(#fa)"/>
        <path class="edge" d="M390 211 L444 128" marker-end="url(#fa)"/>
        <path class="edge" d="M600 128 L654 58" marker-end="url(#fa)"/>
        <path class="edge" d="M600 128 L654 198" marker-end="url(#fa)"/>
        <path class="edge" d="M790 198 H824" marker-end="url(#fa)"/>
        <path class="edge back" d="M890 170 V8 H290 V14" marker-end="url(#fa)"/>
        <text x="590" y="14" class="lbl">next pass starts with the new lessons in the prompt</text>
      </svg></div></div>

    <div class="panel"><h2>Words used on this dashboard</h2><div class="scroll"><table id="plain-glossary"><thead><tr><th>Word</th><th>Plain meaning</th></tr></thead><tbody></tbody></table></div></div>

    <div class="panel"><h2>Design choices</h2>
      <p class="muted">The main choices behind the harness. Each card says what the idea is and how gitagent applies it.</p>
      <div class="cols" id="approaches"></div></div>
    <div class="panel"><h2>Feature switches by profile</h2>
      <p class="muted">Every behaviour that can change results is a switch in a profile file, so it can be measured and turned off if it does not help. The built-in profiles keep unmeasured features off; the bench profiles turn them on for comparison runs. Green = on, red = off.</p>
      <div class="scroll"><table id="switches"></table></div></div>
    <div class="panel"><h2>Profiles (raw files)</h2><div id="profiles-raw"></div></div>
    <div class="panel"><h2>Documents</h2><div id="docs"></div></div>
  </section>
</div>
<script>
"use strict";
const TABS = [["overview","Overview"],["rounds","Rounds & leaderboard"],["compare","gitagent vs Terminus"],["trials","Trials & logs"],["failures","Failures"],["guards","Guards & loops"],["learning","Self-learning"],["tasks","Tasks"],["design","Harness design"]];
const GLOSSARY = {
  turns: ["Model calls made in the run.", ""],
  "tool_calls.*": ["Calls per tool: exec, write_stdin, read_file, write_file, str_replace, apply_patch, image_read, skill, subagent, task_complete.", ""],
  input_tokens: ["Prompt tokens over the run, including cached ones.", ""],
  output_tokens: ["Tokens the model produced, including thinking.", ""],
  cache_read_tokens: ["Prompt tokens served from the provider's cache (cheap).", "cache.breakpoints"],
  cache_write_tokens: ["Prompt tokens written into the cache.", "cache.breakpoints"],
  shaped_bytes_in: ["Raw command output bytes before trimming.", "shape"],
  shaped_bytes_out: ["Bytes actually sent to the model after trimming.", "shape"],
  collapse_events: ["Runs of repeated output lines collapsed into one.", "shape.collapse_min_run"],
  spill_reads: ["Times the agent read a spill file (the full output of an earlier command).", ""],
  snapshot_ok: ["The environment snapshot (OS, tools, files) was added to the first message.", "snapshot"],
  snapshot_failed: ["The snapshot probe failed; the run went on without it.", "snapshot"],
  stop_accepts: ["task_complete accepted by the stop check.", "stop.enabled"],
  stop_rejections: ["task_complete refused because nothing was verified after the last edit.", "stop.enabled"],
  loop_triggers: ["The same tool call gave the same result loop_repeats times; the agent was told to change approach.", "guards.loop_detector"],
  effort_bumps: ["Reasoning effort raised for one call after a loop warning.", "guards.loop_effort_bump"],
  nudges: ["Time-budget reminder at nudge_at of the budget.", "guards.time_budget"],
  forced_stops: ["Time-budget final call at force_stop_at: finish now.", "guards.time_budget"],
  compactions: ["History replaced by a model-written summary to fit the context window.", "context.compaction"],
  compaction_fallbacks: ["The summary request failed; a mechanical summary of recent calls was used.", "context.compaction"],
  overflow_compactions: ["The provider said the prompt was too long; compacted and retried.", "context.compaction"],
  masks_applied: ["Old tool outputs hidden (client or server side) to save context.", "context.masking"],
  mask_checkpoints: ["Batches of masking applied.", "context.masking"],
  truncated_turns: ["Replies cut off at max_output_tokens.", "max_output_tokens"],
  no_tool_turns: ["Replies without a tool call (the agent is nudged; three in a row end the run).", ""],
  repairs: ["Tool calls recovered from plain text (models without native tool calling).", "tool_calling"],
  malformed_calls: ["Tool calls with missing or invalid arguments.", ""],
  refusals: ["The model's safety classifiers refused.", "anthropic.refusal_fallback"],
  fallback_turns: ["Turns served by a fallback model after a refusal.", "anthropic.refusal_fallback"],
  exec_timeouts: ["Commands still running when their wait ended; they kept running in the background.", "exec_default_yield_ms"],
  background_sessions: ["Extra shell sessions started for long-running commands.", ""],
  shell_restarts: ["The shell stopped answering and was restarted.", ""],
  stdin_writes: ["Text written to a running program via write_stdin.", "tools.write_stdin"],
  stdin_polls: ["write_stdin calls that only read output.", "tools.write_stdin"],
  interrupts: ["Ctrl-C sent to a running command.", "tools.write_stdin"],
  str_replace_fallbacks: ["Edits that matched only after ignoring whitespace.", "edit_tool"],
  patch_failures: ["apply_patch calls that failed to apply.", "edit_tool"],
  protected_writes_blocked: ["Writes under /tests refused (integrity rule).", "always on"],
  hook_blocks: ["Tool calls blocked by a hook script.", "hooks"],
  hook_modifies: ["Tool calls or outputs changed by a hook script.", "hooks"],
  subagent_calls: ["Sub-agents started.", "tools.subagents"],
  sdk_named_calls: ["Tool calls the model made under .claude/ tool names (Bash, Read, Edit…) that were mapped to gitagent's tools.", "tool_naming"],
  subagent_turns: ["Turns used inside sub-agents.", "tools.subagents"],
  subagent_parallel_turns: ["Replies in which several sub-agents ran at the same time.", "tools.max_concurrent_subagents"],
  subagent_partial: ["Sub-agents that ran out of turns and returned a partial result.", "tools.subagents"],
  subagent_model_overrides: ["Sub-agents run on a model other than the parent's.", "agents/*.md model"],
  subagent_effort_overrides: ["Sub-agents run at their own reasoning effort.", "agents/*.md effort"],
};
const OUTCOMES = {
  completed: "The agent called task_complete and it was accepted. The checker then decides pass or fail.",
  max_turns: "Every allowed turn (max_turns in gitagent.yaml) was used without finishing.",
  time_up: "gitagent's own time budget ran out before task_complete.",
  no_tool_calls: "Three replies in a row had no tool call; the run was ended.",
  refused: "The model's safety classifiers declined to continue and no fallback model took over.",
  "provider_failed: …": "The model API kept failing after retries (rate limit, outage, bad key).",
};
const EVENT_NAMES = { loop:"loop warning", time_nudge:"time reminder", force_stop:"time: final call", no_tool_nudge:"no-tool nudge",
  compaction:"history compacted", compaction_summary:"compaction summary", stop_rejection:"stop check rejected task_complete",
  protected_write:"/tests write refused", hook_block:"hook blocked a call", subagent:"sub-agent result", next_task:"next task" };
const APPROACHES = [
  ["Terminal-Bench and Harbor", "What it is: a public exam of 89 computer tasks, and the program (Harbor) that runs an agent on them and grades the result with hidden tests. How we use it: the whole scoring setup. Terminus 2 is Harbor's own agent, the one we must beat. Oracle is the answer key, used only to check the machine."],
  ["A short, fixed instruction sheet", "What it is: the standing instructions given to the model before every task. Our choice: keep it short, keep it exactly the same for every task so the model API can cache it (cheaper and faster), and never mention the benchmark in it. Anything about the task at hand goes in the first message instead."],
  ["Environment snapshot, and no tidy-up rule", "What it is: what the model is told at the start and at the end. Our choice: give the model a snapshot of the machine in the first message, and never tell it to tidy up after itself, because tidying deletes things the checker needs. Our learner also reads full run records, not summaries."],
  ["A very small agent", "What it is: a tiny prompt and a handful of tools. Our choice: that minimalism, so the cached prefix stays small and the model has fewer bad choices. Remembering which files were changed when the history gets shortened is still on our to-do list."],
  ["Helper agents", "What it is: letting the agent hand a sub-task to a fresh helper that reports back a short summary, so the main conversation stays small. Our choice: built in, but switched off until measured, because the model tends to over-use helpers."],
  ["apply_patch, a patch-format edit tool", "What it is: a compact way for the model to describe several file edits in one call. Our choice: offer it only in the OpenAI profile, whose models are trained on this style; the Claude profiles use str_replace."],
  ["Lessons with votes", "What it is: letting the agent write itself short pieces of advice after each run and keep score of which ones help. Our choice: a lesson store where each lesson starts with 2 points, gains 1 per new task that supports it, loses 1 when it hurts, and is dropped at 0. Only lessons backed by two different tasks reach the prompt, and every lesson is screened so it cannot smuggle in task answers."],
  ["Honesty rules", "What it is: our own rules so the score means something. Never read or write the hidden tests; never behave differently based on a task's name; learn only from the practice half of the tasks; no internet except the model API during a run; the stop check may only accept or refuse the model's answer, never rewrite it. A script checks the source for all of this."],
];
const PLAIN = [
  ["task", "One problem from the benchmark: an instruction, a prepared machine (Docker image) and a hidden test. There are 89."],
  ["trial / run", "One attempt at one task. Running the same task twice gives two trials."],
  ["job", "One harbor run command; a folder under bench/jobs holding its trials."],
  ["pass", "A batch of dev tasks we ran together (pass 1, pass 2, …)."],
  ["dev split / held-out", "The 89 tasks are divided 45 / 44. Dev tasks are practice: we may run, read and learn from them. Held-out tasks are the real exam: never touched until the final score."],
  ["oracle", "Harbor running the task's official answer script instead of an agent. Should always pass; if it fails, the machine is at fault, not the agent."],
  ["Terminus 2", "Harbor's reference agent. The target is to beat it by 5+ points on the same model."],
  ["verdict / reward", "The hidden checker's result for a trial: 1.0 = pass, 0.0 = fail."],
  ["turn", "One round of the loop: one question to the model and the commands it asked for."],
  ["tool / tool call", "What the model can ask the agent to do: run a command, read or write a file, type into a program, say it is done."],
  ["trajectory", "The full written record of a trial: every turn, command and output. The learner reads these."],
  ["counter", "A number the harness keeps about its own behaviour (turns, loop warnings, tokens…). Every feature has them."],
  ["profile", "A settings file per model family (Anthropic, OpenAI, …) with every feature switch."],
  ["system prompt", "The standing instructions the model sees before every task. Identical for all tasks."],
  ["snapshot", "A quick look at the machine (folders, tools, versions) put into the first message."],
  ["stop check", "A guard that refuses the model's 'done' when it changed something and never tested it afterwards."],
  ["loop detector", "A guard that notices the same command giving the same result again and again."],
  ["time budget", "Reminders to the model when time is running out, and a final 'finish now'."],
  ["compaction", "Shortening the conversation when it gets too long for the model, by replacing old turns with a summary."],
  ["masking", "Hiding old command outputs from the model to save space, keeping only the recent ones."],
  ["cache", "The model API remembers the unchanged start of the conversation so it is not charged again at full price."],
  ["spill file", "The full output of a command, saved to disk when it is too long to show the model in full."],
  ["lesson / learner", "A short piece of general advice written by the learner from our own trials and verdicts; added to the prompt once two tasks support it."],
  ["sub-agent", "A fresh helper agent given one sub-task; only its summary comes back."],
  ["tokens", "The units the model API charges for: roughly 3/4 of a word each."],
];
const SWITCH_ROWS = [["snapshot","Environment snapshot in the first message"],["stop.enabled","Stop check (reject task_complete after unverified edits)"],
  ["guards.time_budget","Time budget: reminders and a final call"],["guards.loop_detector","Loop detector"],["guards.loop_effort_bump","Raise effort after a loop warning"],
  ["context.masking","Hide old tool outputs (none / client / server)"],["context.compaction","Compaction near the context window"],["context.compact_at_tokens","Compact this many tokens before the window"],
  ["lessons","Append learned lessons to the prompt"],["tools.subagents","Sub-agents"],["tools.subagent_depth","How deep sub-agents may nest"],["tools.max_concurrent_subagents","Sub-agents run at once"],["tools.write_stdin","write_stdin tool"],["tools.image_read","image_read tool"],["tools.todo","todo tool"],
  ["tool_naming","Tool names the model sees (gitagent / sdk)"],["edit_tool","Edit tool"],["effort","Reasoning effort"],["max_output_tokens","Max output tokens"],["cache.ttl","Prompt cache lifetime"],["shape.cap_tokens","Output cap per command (tokens)"],["anthropic.refusal_fallback","Fallback model after a refusal"],["anthropic.thinking_binding","Thinking-block binding"]];
const LEARNING_HOW = `
<p><b>Offline (benchmark):</b> <code>gitagent learn --from bench/jobs/&lt;job&gt;</code> reads each trial's own trajectory and its reward, nothing else. For each task it shows the model one accepted and one rejected attempt together when both exist, so it can learn from the difference, and asks for up to three operations: <b>add</b>, <b>edit</b>, <b>upvote</b> or <b>downvote</b> a lesson.</p>
<p><b>Audit:</b> an added or edited lesson is rejected if it names the task, repeats a path, file name, identifier or number from the instruction, contains code, or is too long or too short.</p>
<p><b>Importance:</b> a new lesson starts at 2; a vote from a <i>new</i> task adds 1 (repeated runs of the same task do not count), a downvote subtracts 1, and a lesson is deleted at 0.</p>
<p><b>Shown to the agent:</b> only lessons supported by at least two different tasks, rendered into <code>prompts/lessons.md</code>, which is reviewed and committed like any prompt change. Held-out runs use the committed file frozen.</p>
<p><b>Online (everyday use):</b> <code>learning: online</code> in a project's gitagent.yaml runs the same learner after each completed task, into the project's <code>.gitagent/</code>. Never in <code>--bench</code>.</p>`;

// S.model is the page-wide model filter ("" = all models), kept in the URL hash as #model=;
// S.cmpModel is the compare page's own selector (one model at a time, never mixed).
let S = { data:null, tab:"overview", model:"", cmpModel:null, job:"", kind:"", status:"", trial:null, trialJob:null, detail:null, sub:"turns", timer:null, sorts:{} };
const $ = s => document.querySelector(s);
const esc = t => String(t ?? "").replace(/[&<>"]/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]));
const fmtN = n => n == null ? "—" : n >= 1e6 ? (n/1e6).toFixed(1)+"M" : n >= 1e4 ? Math.round(n/1e3)+"k" : String(Math.round(n));
const fmtS = s => s == null ? "—" : s >= 3600 ? (s/3600).toFixed(1)+"h" : s >= 120 ? (s/60).toFixed(1)+"m" : Math.round(s)+"s";
const when = t => t ? t.slice(0,16).replace("T"," ") : "—";
const pct = (p,n) => n ? Math.round(100*p/n)+"%" : "—";
const pp = x => x == null ? "—" : (100 * x).toFixed(1) + "%";
const STATUS_LABEL = {pass:"pass",fail:"fail",error:"error",running:"running",checking:"checking",no_verdict:"no verdict",abandoned:"stale"};
const badge = s => `<span class="badge b-${s}${s==="running"||s==="checking"?" pulse":""}">${STATUS_LABEL[s]||s}</span>`;
const kind = k => `<span class="badge k-${esc(k.split(",")[0].trim())}">${esc(k)}</span>`;
const diff = d => `<span class="badge k-${esc(d)}">${esc(d)}</span>`;
// How a run ended: the limit outcomes get a warning chip, "completed" stays plain text.
const OUTCOME_CHIP = { time_up:"timeout", max_turns:"max turns", no_tool_calls:"no tool calls", refused:"refused" };
const outc = o => o == null ? '<span class="muted">—</span>' : OUTCOME_CHIP[o] || String(o).startsWith("provider_failed") ? `<span class="badge b-warn" title="${esc(o)}">${esc(OUTCOME_CHIP[o] || "provider failed")}</span>` : `<span class="muted">${esc(o)}</span>`;
const usd = v => v == null ? "—" : v < 0.01 ? "<$0.01" : "$" + v.toFixed(2);
const bar = (p,n,cls="") => `<span class="bar ${cls}"><i style="width:${n?100*p/n:0}%"></i></span>`;
const sm = t => `<span class="sm">${t}</span>`;
const empty = t => `<div class="empty">${t}</div>`;
const allTrials = d => d.jobs.flatMap(j => j.trials.map(t => ({...t, _job:j.name})));
// ---- models: names, chips and the page-wide filter ----
const M = id => (S.data && S.data.models && S.data.models[id]) || { id, label: id || "—", short: id || "—", full: id || "—", index: 2, priced: true, has_rounds: false };
const modelList = d => Object.values(d.models || {}).sort((a, b) => a.index - b.index);
const mchip = (id, title) => id ? `<span class="badge m-${M(id).index % 3}" title="${esc(title || id)}">${esc(M(id).label)}</span>` : '<span class="muted">—</span>';
const inModel = t => !S.model || t.model_id === S.model;
const shownModels = d => modelList(d).filter(m => !S.model || m.id === S.model);
// Costs: a run on a model bench/pricing.json does not price has an unknown cost, shown as n/a, never $0.
const NA = '<span class="muted" title="no price for this model in bench/pricing.json">n/a</span>';
const trialCost = t => t.cost_usd == null && (t.input_tokens || 0) > 0 ? NA : usd(t.cost_usd);
const costCell = (cost, unpriced) => unpriced ? (cost ? `${usd(cost)} <span class="muted" title="${unpriced} run(s) on an unpriced model not included">+ n/a</span>` : NA) : usd(cost);
function hashFor() {
  const parts = [];
  if (!["trial", "round"].includes(S.tab)) parts.push("tab=" + S.tab);
  if (S.model) parts.push("model=" + encodeURIComponent(S.model));
  if (S.job && S.tab === "trials") parts.push("job=" + encodeURIComponent(S.job));
  return parts.length ? "#" + parts.join("&") : location.pathname;
}
function setModel(id) {
  S.model = id || "";
  if (S.model) S.cmpModel = S.model;
  if (!["trial", "round"].includes(S.tab)) history.replaceState(null, "", hashFor());
  render();
}

function tabs() {
  const d = S.data;
  const counts = { trials: d ? d.totals.trials : "", failures: d ? allTrials(d).filter(t => !["pass","running"].includes(t.status)).length : "",
    tasks: d ? d.tasks.length : "", learning: d && d.lessons ? d.lessons.lessons.length : "" };
  const tabList = [...TABS, ...(S.round ? [["round", "Round: " + S.round]] : []), ...(S.trial ? [["trial", "Trial: " + S.trial]] : [])];
  $("#tabs").innerHTML = tabList.map(([id,label]) => `<button class="tab" role="tab" data-tab="${id}" aria-selected="${S.tab===id}">${esc(label)}${counts[id]!==undefined && counts[id]!=="" ? `<span class="cnt">${counts[id]}</span>`:""}</button>`).join("");
  for (const [id] of [...TABS, ["trial"], ["round"]]) $(`#v-${id}`).hidden = S.tab !== id;
}

// Open <details> survive the live refresh: remember them by panel and label.
const detailKey = el => el.dataset.key || ((el.closest("section")?.id || "") + "|" + (el.querySelector("summary")?.textContent || ""));
function render() {
  const d = S.data; if (!d) return;
  const open = new Set([...document.querySelectorAll("details[open]")].map(detailKey));
  const scrollY = window.scrollY;
  renderInner(d);
  if (open.size) for (const el of document.querySelectorAll("details")) if (open.has(detailKey(el))) el.open = true;
  postRender();
  window.scrollTo(0, scrollY);
}
function renderInner(d) {
  tabs();
  $("#meta").textContent = `${d.root} · updated ${d.generated_at.slice(11,19)} UTC · refreshes every 4s`;
  const t = d.totals, P = d.progress, models = modelList(d);
  const bm = id => P.by_model[id] || {};
  const tm = id => t.by_model[id] || {};
  const kpi = (big, lbl, exp, cls = "", attrs = "") => `<div class="stat kpi ${cls}" ${attrs}><span class="big">${big}</span><span class="lbl">${lbl}</span><span class="exp">${exp}</span></div>`;
  // One held-out tile per model that has a held-out run, one current-round tile per model.
  const heldTiles = models.filter(m => bm(m.id).heldout).map(m => { const h = bm(m.id).heldout;
    return kpi(`${(100 * h.accuracy).toFixed(1)}%`, `Final, held-out ${mchip(m.id)}`, `gitagent · ${esc(m.label)} on ${h.done} unseen tasks, ${h.runs / Math.max(h.done, 1)} runs each · single runs ${h.per_run.map(x => (100 * x.accuracy).toFixed(1) + "%").join(" / ")} · open round ${esc(h.round)}`, "final click", `data-round="${esc(h.round)}" title="Open the held-out round"`); });
  if (!heldTiles.length) heldTiles.push(kpi("—", "Final, held-out", `Not measured yet: the ${d.accuracy.heldout_total} held-out tasks stay unseen until the final scoring run.`, "final"));
  // One tile per model that has a run over all tasks (the leaderboard format).
  const fullTiles = models.filter(m => bm(m.id).full).map(m => { const f = bm(m.id).full;
    const live = (f.running || []).length, open = (f.pending || []).length;
    return kpi(f.accuracy == null ? "—" : `${(100 * f.accuracy).toFixed(1)}%`, `Full run, all ${f.scope} tasks ${mchip(m.id)}`,
      `gitagent · ${esc(m.label)} · ${f.done}/${f.scope} tasks scored${f.runnable_accuracy != null ? ` · ${(100 * f.runnable_accuracy).toFixed(1)}% on tasks this machine can run` : ""}${live ? ` · ${live} running` : ""}${open ? ` · ${open} pending` : ""} · open round ${esc(f.round)}`,
      "final click", `data-round="${esc(f.round)}" title="Open the full run"`); });
  const curTiles = models.map(m => { const g = bm(m.id).gitagent || {}, r = g.round, b = bm(m.id);
    if (!r) return kpi("—", `Current round ${mchip(m.id)}`, `${esc(m.label)}: no rounds yet${b.unrounded_trials ? ` · ${b.unrounded_trials} trial(s) in jobs no round in bench/rounds.json lists yet (${(b.unrounded_jobs || []).map(esc).join(", ")})` : ""}`);
    return kpi(g.accuracy == null ? "—" : Math.round(100 * g.accuracy) + "%", `Current round ${mchip(m.id)} · ${esc(r.name)}`,
      `gitagent · ${esc(m.label)} ${g.passed}/${g.done} tasks · ${g.done}/${g.scope} done${g.running.length ? ` · ${g.running.length} running` : ""} · ${g.pending.length} pending`,
      "click", `data-round="${esc(r.name)}" title="Open round ${esc(r.name)}"`); });
  if (!curTiles.length) curTiles.push(kpi("—", "Current round", "No model-calling run yet."));
  const perModel = f => models.map(m => `${esc(m.short)} ${f(m)}`).join(", ");
  const liveBy = perModel(m => `${tm(m.id).running || 0}${tm(m.id).running ? ` (${usd(tm(m.id).live_cost_usd)})` : ""}`);
  const spendBy = perModel(m => costCell(tm(m.id).cost_usd, tm(m.id).unpriced_runs));
  const unpriced = t.unpriced_models || [];
  $("#kpis").innerHTML = [
    ...fullTiles, ...heldTiles, ...curTiles,
    kpi(`${t.running} ${sm("running")}`, "Live trials", t.running ? `${usd(t.live_cost_usd)} spent so far in running trials${models.length > 1 ? ` · ${liveBy}` : ""}${t.runaway.length ? ` · ${t.runaway.length} over the ${usd(t.runaway_usd)} runaway limit` : ""}` : "Nothing running right now.", t.runaway.length ? "alarm" : ""),
    kpi(usd(t.cost_usd), "Estimated spend", `${models.length ? spendBy + " · " : ""}${t.jobs} jobs · ${t.trials} trials${unpriced.length ? ` · <span class="bad">no price for ${unpriced.map(esc).join(", ")} in bench/pricing.json: that spend shows as n/a and is not in the total</span>` : ""}`, unpriced.length ? "alarm" : ""),
  ].join("");
  const termChips = models.filter(m => (bm(m.id).terminus || {}).runs).map(m => { const p = bm(m.id).terminus;
    return `terminus 2 ${mchip(m.id)} <b>${p.accuracy == null ? "—" : Math.round(100 * p.accuracy) + "%"}</b> (${p.passed}/${p.done} tasks) · ${p.done}/${p.scope} done${p.running.length ? ` · ${p.running.length} running` : ""} · ${p.pending.length} pending`; });
  const o = P.oracle;
  $("#chips").innerHTML = [
    ...termChips,
    `oracle (machine check) <b>${o.accuracy == null ? "—" : Math.round(100 * o.accuracy) + "%"}</b> (${o.passed}/${o.done} tasks) · ${o.done}/${o.scope} done${o.running.length ? ` · ${o.running.length} running` : ""} · ${o.pending.length} pending`,
    `jobs <b>${t.jobs}</b> · trials <b>${t.trials}</b> · running <b>${t.running}</b>`,
    ...models.map(m => { const x = tm(m.id);
      return `${mchip(m.id)} spent <b>${costCell(x.cost_usd, x.unpriced_runs)}</b> · tokens in <b>${fmtN(x.input_tokens)}</b> · cached <b>${fmtN(x.cache_tokens)}</b> · out <b>${fmtN(x.output_tokens)}</b>${x.running ? ` · <b>${x.running}</b> running (<b>${usd(x.live_cost_usd)}</b>)` : ""}`; }),
  ].map(c => `<span class="chip">${c}</span>`).join("") +
    unpriced.map(m => `<span class="chip chip-alarm">no price for <b>${esc(m)}</b> in bench/pricing.json: its costs show as n/a</span>`).join("") +
    t.runaway.map(r => `<span class="chip chip-alarm">runaway? <b>${esc(r.kind)} · ${esc(r.task)}</b> ${mchip(r.model_id)} has used <b>${usd(r.cost_usd)}</b> while still running (over ${usd(t.runaway_usd)}) · <a href="#" data-open="${esc(r.job)}|${esc(r.trial)}">open</a></span>`).join("");
  renderOverview(d); renderRounds(d); renderRound(d); renderCompare(d); renderTrials(d); renderFailures(d); renderGuards(d); renderLearning(d); renderTasks(d); renderDesign(d);
}

// ---- generic table helpers: right-align numeric headings, sort on heading click ----
function alignNumerics(table) {
  const head = table.tHead; if (!head || head.rows.length !== 1) return;
  const body = table.tBodies[0]; if (!body) return;
  const probe = [...body.rows].slice(0, 3).filter(r => !r.querySelector("[colspan]"));
  [...head.rows[0].cells].forEach((th, i) => { if (probe.length && probe.every(r => r.cells[i] && r.cells[i].classList.contains("num"))) th.classList.add("num"); });
}
function sortVal(td) {
  const t = (td ? td.textContent : "").trim();
  if (!t || t === "—") return { n: -Infinity, s: "" };
  if (/^\d{4}-\d\d-\d\d/.test(t)) return { n: NaN, s: t };
  const m = t.match(/^[+\-−$<]*\s*(\d[\d,]*\.?\d*)\s*([kMhms%]?)/);
  if (!m) return { n: NaN, s: t.toLowerCase() };
  let v = parseFloat(m[1].replace(/,/g, "")); const u = m[2];
  if (u === "k") v *= 1e3; else if (u === "M") v *= 1e6; else if (u === "h") v *= 3600; else if (u === "m") v *= 60;
  if (/^[\-−]/.test(t)) v = -v;
  return { n: v, s: t.toLowerCase() };
}
function applySort(table) {
  const st = S.sorts[table.id]; const head = table.tHead; const body = table.tBodies[0];
  if (!head || !body) return;
  for (const th of head.querySelectorAll("th")) th.removeAttribute("aria-sort");
  if (!st) return;
  const th = head.rows[0] && head.rows[0].cells[st.col]; if (!th) return;
  th.setAttribute("aria-sort", st.dir > 0 ? "ascending" : "descending");
  const rows = [...body.rows].map((r, i) => ({ r, i, v: sortVal(r.cells[st.col]) }));
  rows.sort((a, b) => { const an = a.v.n, bn = b.v.n; let c;
    if (!Number.isNaN(an) && !Number.isNaN(bn)) c = an - bn; else if (Number.isNaN(an) !== Number.isNaN(bn)) c = Number.isNaN(an) ? 1 : -1; else c = a.v.s.localeCompare(b.v.s);
    return (c || a.i - b.i) * (c ? st.dir : 1); });
  for (const { r } of rows) body.appendChild(r);
}
function postRender() {
  for (const table of document.querySelectorAll("table")) { alignNumerics(table); if (table.classList.contains("sortable")) applySort(table); }
}

function renderModelFilter(d) {
  const models = modelList(d);
  $("#model-filter").innerHTML = models.length ? `<span class="muted">model</span><div class="subtabs">` +
    [["", "All"], ...models.map(m => [m.id, m.label])].map(([id, l]) => `<button class="subtab" data-model="${esc(id)}" aria-selected="${S.model === id}">${esc(l)}</button>`).join("") +
    `</div><span class="muted">${S.model ? `showing ${esc(M(S.model).label)} only (${esc(S.model)}); the compare page follows this choice` : "every model; rows and cards stay apart per model"}</span>` : "";
}

function renderProgress(d) {
  const P = d.progress, models = modelList(d);
  const list = (xs, cls = "") => xs.length ? `<details><summary>${xs.length}</summary><span class="muted ${cls}">${xs.map(esc).join(", ")}</span></details>` : '<span class="muted">0</span>';
  // One row per (harness, model): held-out runs first, then each model's current round, Terminus per model, the oracle once.
  const rows = [];
  for (const m of models) { const b = P.by_model[m.id] || {};
    if (b.heldout) rows.push({ k: "gitagent", model: m.id, label: `gitagent · ${m.label} (held-out)`, p: b.heldout, rn: b.heldout.round, sel: true }); }
  for (const m of models) { const b = P.by_model[m.id] || {}, g = b.gitagent || {};
    rows.push({ k: "gitagent", model: m.id, label: g.round ? `gitagent · ${m.label}, round ${g.round.name}` : `gitagent · ${m.label}`, p: g, rn: (g.round || {}).name, none: !g.round, note: b.unrounded_trials ? `${b.unrounded_trials} trial(s) in jobs no round lists yet` : "" }); }
  for (const m of models) { const b = P.by_model[m.id] || {};
    if ((b.terminus || {}).runs) rows.push({ k: "terminus", model: m.id, label: `Terminus 2 · ${m.label}`, p: b.terminus, rn: b.terminus_round }); }
  rows.push({ k: "oracle", model: null, label: "oracle (machine check)", p: P.oracle, rn: (P.history.filter(h => h.kind === "oracle").pop() || {}).round });
  const shown = rows.filter(r => !r.model || inModel({ model_id: r.model }));
  $("#progress").innerHTML = `<thead><tr><th>Harness</th><th>Model</th><th>Scope</th><th class="num">Accuracy</th><th class="num">Runnable accuracy</th><th class="num">Done</th><th></th><th class="num">All runs passed</th><th>Some run failed</th><th>Running</th><th>Pending</th><th class="num">Runs</th></tr></thead><tbody>` +
    (shown.length ? shown.map(r => { const p = r.p;
      if (r.none) return `<tr><td>${kind(r.k)} <b>${esc(r.label)}</b></td><td>${mchip(r.model)}</td><td class="muted" colspan="10">no rounds yet${r.note ? ` · ${esc(r.note)}` : ""}</td></tr>`;
      return `<tr class="${r.sel ? "sel" : ""} ${r.rn ? "click" : ""}" ${r.rn ? `data-round="${esc(r.rn)}" title="Open round ${esc(r.rn)}"` : ""}><td>${kind(r.k)} <b>${esc(r.label)}</b></td><td>${r.model ? mchip(r.model) : '<span class="muted">no model</span>'}</td><td class="muted">${p.scope} ${esc(p.scope_label)}</td>
        <td class="num"><b>${pp(p.accuracy)}</b></td><td class="num">${pp(p.runnable_accuracy)} <span class="muted">(${p.runnable_done})</span></td>
        <td class="num">${p.done}/${p.scope}</td><td>${bar(p.done, p.scope, "b-acc")}</td><td class="num">${p.passed}</td>
        <td>${list(p.failed)}</td><td>${list(p.running)}</td><td>${list(p.pending)}</td><td class="num">${p.runs}</td></tr>`; }).join("")
    : `<tr><td colspan="12" class="muted">No harness has run on this model yet.</td></tr>`) + "</tbody>";
  const hist = P.history.filter(h => !S.model || h.model === S.model);
  $("#history").innerHTML = `<thead><tr><th>Round</th><th>Harness</th><th>Model</th><th>Host</th><th>Dates (UTC)</th><th>Accuracy</th><th>Runnable accuracy</th><th>Done</th><th>Passed</th><th>Runs</th><th>Timeouts</th><th>Median time</th><th>Cost</th><th>Cost / run</th></tr></thead><tbody>` +
    (hist.length ? hist.map(h => `<tr class="click ${h.current ? "sel" : ""}" data-round="${esc(h.round)}" title="Open round ${esc(h.round)}"><td class="wrap-s"><b>${esc(h.round)}</b>${h.current ? ' <span class="badge b-running">current</span>' : ""}${h.split === "held-out" ? ' <span class="badge">held-out</span>' : ""}<br><span class="muted">${esc(h.label)}${h.commit ? ` · ${esc(h.commit)}` : ""}</span></td>
      <td>${kind(h.kind)}</td><td>${h.model ? mchip(h.model) : '<span class="muted">no model</span>'}${h.model_mismatch ? ` <span class="badge b-warn" title="bench/rounds.json says this round ran ${esc(h.round_model)}">round says ${esc(M(h.round_model).label)}</span>` : ""}</td><td class="muted">${esc(h.host || "—")}</td><td class="muted dt">${h.first ? h.first.slice(0,16).replace("T"," ") : "—"}${h.last && h.last.slice(0,10) !== (h.first||"").slice(0,10) ? " → " + h.last.slice(5,16).replace("T"," ") : ""}</td>
      <td class="num"><b>${pp(h.accuracy)}</b></td><td class="num">${pp(h.runnable_accuracy)} <span class="muted">(${h.runnable_done})</span></td>
      <td class="num">${h.done}/${h.scope}${h.running.length ? ` <span class="badge b-running pulse">${h.running.length} live</span>` : ""}</td><td class="num">${h.passed}</td><td class="num">${h.runs}</td>
      <td class="num">${h.timeouts}</td><td class="num">${fmtS(h.median_agent_s)}</td><td class="num">${costCell(h.cost_usd, h.unpriced_runs)}</td><td class="num">${h.unpriced_runs ? NA : usd(h.cost_per_run)}</td></tr>`).join("")
    : `<tr><td colspan="14" class="muted">No round has run on this model yet.</td></tr>`) + "</tbody>";
}

function renderOverview(d) {
  renderModelFilter(d);
  renderProgress(d);
  const t = d.totals, stat = card, models = shownModels(d);
  const group = (m, cards) => `<div><h3>${mchip(m.id)} ${m.full !== m.label ? esc(m.full) : ""} <span class="muted mono">${esc(m.id)}</span></h3><div class="cols">${cards}</div></div>`;
  // Accuracy cards, one set per model: a dev or held-out number never spans two models.
  const accCards = m => { const a = d.accuracy.by_model[m.id] || {}, tmm = t.by_model[m.id] || {}, b = d.progress.by_model[m.id] || {}, h = b.heldout;
    const diffs = Object.entries(a.by_difficulty || {}).map(([k,v]) => `${k} ${v.passed}/${v.attempted}`).join(" · ") || "—";
    return [
      stat(`${pct(a.dev_runnable_passed, a.dev_runnable)} ${sm(`(${a.dev_runnable_passed}/${a.dev_runnable})`)}`,
        `Dev accuracy on tasks this machine can run ${mchip(m.id)}`,
        (a.machine_blocked || []).length
          ? `The headline dev number for ${esc(m.label)}. Leaves out ${a.machine_blocked.length} task(s) whose own reference solution also fails on this machine (oracle runs): ${a.machine_blocked.map(esc).join(", ")}. A failure there is the machine's (emulation, missing CPU features), not the agent's.`
          : `Every dev task ${esc(m.label)} attempted can run on this machine (no oracle failures among them), so this equals the by-task number.`, "final"),
      stat(`${pct(a.dev_latest_passed, a.dev_attempted)} ${sm(`(${a.dev_latest_passed}/${a.dev_attempted})`)}`,
        `Dev accuracy, by task (all attempted) ${mchip(m.id)}`,
        `Each dev task counted once, using its most recent ${esc(m.label)} run, including tasks the machine cannot run. ${a.dev_attempted} of ${a.dev_total} dev tasks attempted so far; ${a.dev_total - a.dev_attempted} still to run. By difficulty: ${diffs}. One run per task, so single results are noisy.`),
      stat(`${pct((tmm.agent || {}).passed, (tmm.agent || {}).scored)} ${sm(`(${(tmm.agent || {}).passed ?? 0}/${(tmm.agent || {}).scored ?? 0})`)}`,
        `Agent accuracy, by run ${mchip(m.id)}`,
        `Every scored gitagent run on ${esc(m.label)}, including repeats of the same task and plumbing checks. Higher than the by-task number when a task was run several times and kept passing.`),
      h ? stat(`${(100 * h.accuracy).toFixed(1)}% ${sm(`(${h.passed}/${h.done} every run)`)}`, `Final accuracy (held-out) ${mchip(m.id)}`,
          `The honest score for ${esc(m.label)}: ${h.done} tasks gitagent was never run, tuned or learned on, ${h.runs / Math.max(h.done, 1)} runs each (${h.runs} trials). Single runs: ${h.per_run.map(x => `run ${x.run} ${(100 * x.accuracy).toFixed(1)}% (${x.passed}/${x.tasks})`).join(", ")}. Runnable ${(100 * h.runnable_accuracy).toFixed(1)}%. Round ${esc(h.round)}; see the Rounds & leaderboard tab for the published results it is compared with.`, "final")
        : stat(a.heldout_runs === 0 ? "—" : "!", `Final accuracy (held-out) ${mchip(m.id)}`,
          a.heldout_runs === 0
            ? `Not measured yet for ${esc(m.label)}. The ${d.accuracy.heldout_total} held-out tasks stay unseen until the final scoring run, so the number cannot be inflated by tuning. Target: beat Terminus 2 by 5+ points on the same model.`
            : `${a.heldout_runs} ${esc(m.label)} run(s) touched held-out tasks outside a held-out round: that must not happen before the final scoring run.`, "final"),
      stat((tmm.terminus || {}).scored ? pct(tmm.terminus.passed, tmm.terminus.scored) : "—", `Terminus 2 baseline ${mchip(m.id)}`,
        (tmm.terminus || {}).scored ? `${tmm.terminus.passed}/${tmm.terminus.scored} runs. Harbor's reference agent on ${esc(m.label)} and the same tasks; the ${esc(m.label)} dev number is compared against this.` : `Not run on ${esc(m.label)} yet. Harbor's reference agent on the same model is the comparison point for every gitagent number.`),
    ].join(""); };
  $("#accuracy").innerHTML = models.map(m => group(m, accCards(m))).join("") +
    `<div><h3>Machine check (no model)</h3><div class="cols">${stat(pct(t.oracle.passed, t.oracle.scored), "Machine check (oracle)",
      `${t.oracle.passed}/${t.oracle.scored} runs of the tasks' own reference solutions. Should be 100%; anything less means the machine (emulation, memory, time) fails tasks on its own. Not an agent score and not tied to a model.`)}</div></div>`;
  // Cost: the total is money and may add up across models; everything per run or per token is per model.
  const unpriced = t.unpriced_models || [];
  const costCards = m => { const x = t.by_model[m.id] || {};
    const uncached = Math.max(0, (x.input_tokens || 0) - (x.cache_tokens || 0) - (x.cache_write_tokens || 0));
    const perPass = (x.agent || {}).passed && !x.unpriced_runs ? x.cost_usd / x.agent.passed : null;
    const perRun = (x.agent || {}).scored && !x.unpriced_runs ? x.cost_usd / x.agent.scored : null;
    return [
      stat(costCell(x.cost_usd, x.unpriced_runs), `Estimated cost ${mchip(m.id)}`,
        x.unpriced_runs ? `${x.unpriced_runs} run(s) on ${esc(m.label)} have no price in bench/pricing.json, so this model's spend is unknown (n/a), not zero.`
          : `${esc(m.label)} runs (${Object.entries(x.cost_by_kind || {}).map(([k,v]) => `${k} ${usd(v)}`).join(", ") || "none yet"}) at the ${esc(m.id)} prices in bench/pricing.json.${x.running ? ` ${usd(x.live_cost_usd)} of it in the ${x.running} running trial(s).` : ""}`),
      stat(`${x.unpriced_runs ? NA : usd(perRun)} ${sm("/ run")}`, `Cost per run and per pass ${mchip(m.id)}`,
        `${x.unpriced_runs ? NA : usd(perPass)} per passed task on ${esc(m.label)}. Total cost divided by scored runs and by passes; compares harness versions on the same model only.`),
      stat(fmtN(x.input_tokens), `Input tokens ${mchip(m.id)}`,
        `${fmtN(x.cache_tokens)} read from the cache (${pct(x.cache_tokens, x.input_tokens)}), ${fmtN(x.cache_write_tokens)} written to it, ${fmtN(uncached)} at full price. The blueprint target is ≥80% served from the cache; cache reads are the cheap part of input.`),
      stat(fmtN(x.output_tokens), `Output tokens ${mchip(m.id)}`,
        `Tokens ${esc(m.label)} wrote, including its thinking. Output is the expensive side, so turns and verbosity matter more than prompt size.`),
    ].join(""); };
  $("#cost").innerHTML = `<div><div class="cols">${stat(usd(t.cost_usd), "Estimated total cost, all models",
      `${modelList(d).map(m => `${esc(m.short)} ${costCell((t.by_model[m.id] || {}).cost_usd, (t.by_model[m.id] || {}).unpriced_runs)}`).join(" · ") || "none yet"}, from bench/pricing.json (priced: ${d.totals.priced_models.join(", ") || "none"}; per million tokens). ${unpriced.length ? `No price for ${unpriced.map(esc).join(", ")}: those runs are not in this total.` : "Every run is priced."} Harbor does not report cost itself.`, unpriced.length ? "alarm" : "")}
    ${stat(usd(t.live_cost_usd), "Spending now", t.running ? `What the ${t.running} running trial(s) have used so far (${modelList(d).map(m => `${esc(m.short)} ${usd((t.by_model[m.id] || {}).live_cost_usd)}`).join(", ")}), updated every 4 seconds. Already included in the total.` : "Nothing running.")}</div></div>` +
    models.map(m => group(m, costCards(m))).join("");
  const live = allTrials(d).filter(t => (t.status === "running" || t.status === "checking") && inModel(t));
  $("#live").innerHTML = live.length ? live.map(t => `
    <div class="row"><span>${kind(t.kind)} ${t.kind === "oracle" ? "" : mchip(t.model_id, t.model)} <b>${esc(t.task)}</b></span>
      <span>${badge(t.status)} turn ${t.turns ?? 0}${t.live ? ` · step ${t.live.step} <span class="cmd">${esc(t.live.text)}</span>` : " · checker running"}</span>
      <span class="num muted">${t.kind !== "oracle" ? trialCost(t) + " · " : ""}${fmtS(t.elapsed_s)} elapsed · idle ${fmtS(t.live?.idle_s)} · <a href="#" data-open="${esc(t._job)}|${esc(t.trial)}">open</a></span></div>`).join("")
    : empty(S.model ? `Nothing running on ${esc(M(S.model).label)}.` : `Nothing running. Start a run with <code>harbor run …</code> and it appears here within 4 seconds.`);
  const jobs = d.jobs.filter(j => !S.model || j.model_ids.includes(S.model));
  $("#jobs tbody").innerHTML = jobs.length ? jobs.map(j => `
    <tr class="click ${S.job===j.name?"sel":""}" data-job="${esc(j.name)}">
      <td><b>${esc(j.name)}</b></td><td class="muted dt">${when(j.started)}</td><td>${kind(j.kind)}</td>
      <td>${j.model_ids.length ? j.model_ids.map(m => mchip(m, j.model)).join(" ") : `<span class="muted">${esc(j.model)}</span>`}</td><td class="muted">${esc(j.profile)}</td>
      <td class="num">${j.n_trials}${j.running?` <span class="badge b-running pulse">${j.running} live</span>`:""}${j.errors?` <span class="badge b-error">${j.errors} err</span>`:""}</td>
      <td class="num">${j.passed}/${j.scored}</td><td class="num">${pct(j.passed,j.scored)}</td><td>${bar(j.passed,j.scored)}</td>
      <td class="num">${(j.harbor || {}).evals && j.harbor.evals.length ? j.harbor.evals.map(e => `${pp(e.mean)}${meanGap(e)}`).join("<br>") : `<span class="muted" title="${(j.harbor || {}).finished ? "no mean in the job's result.json" : "the job has not written its result.json yet"}">—</span>`}</td>
      <td class="num">${costCell(j.cost_usd, j.unpriced_runs)}</td></tr>`).join("")
    : `<tr><td colspan="11" class="muted">${S.model ? `No job ran ${esc(M(S.model).label)} yet.` : `No jobs under ${esc(d.root)} yet.`}</td></tr>`;
  // One row per (task, harness, model); the oracle column is the machine check for the task.
  const taskRows = [];
  for (const r of d.tasks) {
    const oracle = `${r.oracle_attempts ? `${r.oracle_passed}/${r.oracle_attempts}` : "—"}${r.machine_blocked ? ' <span class="badge b-error" title="the reference solution fails on this machine">machine fails</span>' : ""}`;
    const slots = Object.entries(r.per_model || {}).filter(([m]) => inModel({ model_id: m })).sort(([a], [b]) => M(a).index - M(b).index);
    for (const [m, s] of slots) {
      if (s.agent_attempts) taskRows.push({ r, k: "gitagent", m, a: s.agent_attempts, p: s.agent_passed, oracle });
      if (s.terminus_attempts) taskRows.push({ r, k: "terminus", m, a: s.terminus_attempts, p: s.terminus_passed, oracle });
    }
    if (!slots.length && r.oracle_attempts && !S.model) taskRows.push({ r, k: "oracle", m: null, a: null, p: null, oracle });
  }
  $("#taskacc tbody").innerHTML = taskRows.length ? taskRows.map(({ r, k, m, a, p, oracle }) => `
    <tr><td><b>${esc(r.task)}</b></td><td>${diff(r.difficulty)}</td><td>${kind(k)}</td><td>${m ? mchip(m) : '<span class="muted">no model</span>'}</td>
    <td class="num">${a ?? "—"}</td><td class="num">${p ?? "—"}</td><td class="num">${a ? pct(p, a) : "—"}</td><td>${a ? bar(p, a) : ""}</td>
    <td class="num muted">${oracle}</td></tr>`).join("")
    : `<tr><td colspan="9" class="muted">${S.model ? `No task has been run on ${esc(M(S.model).label)} yet.` : "No task has been run yet."}</td></tr>`;
}

function card(big, lbl, exp, cls="") {
  return `<div class="stat ${cls}"><span class="big">${big}</span><span class="lbl">${lbl}</span><span class="exp">${exp}</span></div>`;
}
function pill(state) {
  return state === null ? '<span class="badge b-no_verdict">no data yet</span>' : state ? '<span class="badge b-pass">met</span>' : '<span class="badge b-fail">not met</span>';
}
function renderCompare(d) {
  // One model at a time: the selector picks it (default: the model with the most rounds), and
  // every number on the page comes from that model's runs only.
  const all = d.compare, ids = all.models || [];
  if (!S.cmpModel || !ids.includes(S.cmpModel)) S.cmpModel = ids.includes(all.default_model) ? all.default_model : (ids[0] || null);
  const sel = $("#c-model");
  sel.innerHTML = ids.length ? ids.map(id => `<option value="${esc(id)}" ${id === S.cmpModel ? "selected" : ""}>${esc(M(id).full)} (${esc(id)})</option>`).join("") : `<option value="">no model has run yet</option>`;
  const c = (S.cmpModel && all.by_model[S.cmpModel]) || all, g = c.paired.gitagent, t = c.paired.terminus, ga = c.all.gitagent, ta = c.all.terminus;
  const ml = S.cmpModel ? M(S.cmpModel).label : "—";
  $("#cmp-model").innerHTML = S.cmpModel ? `${mchip(S.cmpModel)} both harnesses on ${esc(M(S.cmpModel).full)} only · ${(all.by_model[S.cmpModel] || {}).gitagent_round ? `${ga.runs} gitagent runs` : "no gitagent round"} · ${ta.runs} Terminus runs` : "";
  const both = c.paired_tasks > 0;
  $("#cmp-round").textContent = c.gitagent_round ? `round ${c.gitagent_round.name}: ${c.gitagent_round.label}` : `no ${ml} round yet`;
  const lead = both && g.task_pass_rate != null && t.task_pass_rate != null ? 100 * (g.task_pass_rate - t.task_pass_rate) : null;
  $("#targets").innerHTML = [
    card(lead == null ? "—" : (lead >= 0 ? "+" : "") + lead.toFixed(1) + " pts", `Pass rate vs Terminus 2 ${mchip(S.cmpModel)} ${pill(lead == null ? null : lead >= 5)}`,
      both ? `gitagent ${pp(g.task_pass_rate)} vs Terminus ${pp(t.task_pass_rate)} on ${c.paired_tasks} tasks both ran on ${esc(ml)} (${c.wins} won, ${c.losses} lost, ${c.paired_tasks - c.wins - c.losses} tied). Target: +5 points or more.`
           : `Terminus 2 has not been run on ${esc(ml)} yet${ga.runs ? "" : ", and neither has a gitagent round"}. Run both on the same dev tasks with ${esc(ml)} to fill this in. Target: +5 points or more.`, "final"),
    card(both ? fmtS(g.median_agent_s) : fmtS(ga.median_agent_s), `Median time per task ${pill(both && g.median_agent_s != null && t.median_agent_s != null ? g.median_agent_s <= t.median_agent_s : null)}`,
      both ? `gitagent ${fmtS(g.median_agent_s)} vs Terminus ${fmtS(t.median_agent_s)}, agent time only (setup and the checker excluded). Target: no slower than Terminus.` : `gitagent's median over ${ga.scored} scored runs. Target: no slower than Terminus.`),
    card(pp(ga.timeout_rate), `Timeouts ${pill(ga.timeout_rate == null ? null : ga.timeout_rate < 0.10)}`,
      `${ga.timeouts} of gitagent's runs hit the time limit (Harbor's or gitagent's own budget)${ta.runs ? `; Terminus ${pp(ta.timeout_rate)}` : ""}. Target: under 10%.`),
    card(pp(ga.cache_share), `Input served from the cache ${pill(ga.cache_share == null ? null : ga.cache_share >= 0.80)}`,
      `${fmtN(ga.cache_tokens)} of ${fmtN(ga.input_tokens)} input tokens${ta.runs ? `; Terminus ${pp(ta.cache_share)}` : ""}. Cached input costs about a twentieth of fresh input. Target: 80% or more.`),
    card(both ? usd(g.cost_per_pass) : usd(ga.cost_per_pass), "Cost per passed task",
      both ? `gitagent ${usd(g.cost_per_pass)} vs Terminus ${usd(t.cost_per_pass)} on the shared tasks; per run ${usd(g.cost_per_run)} vs ${usd(t.cost_per_run)}. No target; lower is better at the same pass rate.` : `${usd(ga.cost_per_run)} per run over ${ga.scored} runs. No target; lower is better at the same pass rate.`),
    card(both ? fmtN(g.tokens_per_run) : fmtN(ga.tokens_per_run), "Tokens per run",
      both ? `gitagent ${fmtN(g.tokens_per_run)} vs Terminus ${fmtN(t.tokens_per_run)} (input + output, cached included). Mean turns ${g.mean_turns?.toFixed(1) ?? "—"} vs ${t.mean_turns?.toFixed(1) ?? "—"}.` : `Input + output per run, cached included. Mean turns ${ga.mean_turns?.toFixed(1) ?? "—"}.`),
  ].join("");
  const rowsOverall = [["Tasks with scored runs", s => s.tasks], ["Scored runs", s => s.scored], ["Passed runs", s => s.passed],
    ["Pass rate (mean of per-task rates)", s => pp(s.task_pass_rate)], ["Median agent time", s => fmtS(s.median_agent_s)], ["Mean turns", s => s.mean_turns?.toFixed(1) ?? "—"],
    ["Timeouts", s => `${s.timeouts} (${pp(s.timeout_rate)})`], ["Input tokens", s => fmtN(s.input_tokens)], ["  of which from the cache", s => `${fmtN(s.cache_tokens)} (${pp(s.cache_share)})`],
    ["Output tokens", s => fmtN(s.output_tokens)], ["Cost", s => usd(s.cost_usd)], ["Cost per run", s => usd(s.cost_per_run)], ["Cost per passed task", s => usd(s.cost_per_pass)]];
  $("#cmp-overall").innerHTML = `<thead><tr><th>${mchip(S.cmpModel)}</th><th class="num">gitagent · ${esc(ml)}</th><th class="num">Terminus 2 · ${esc(ml)}</th></tr></thead><tbody>` +
    rowsOverall.map(([l, f]) => `<tr><td>${esc(l)}</td><td class="num">${ga.runs ? f(ga) : "—"}</td><td class="num">${ta.runs ? f(ta) : "—"}</td></tr>`).join("") + "</tbody>";

  let rows = c.rows.slice();
  if ($("#c-paired").checked) rows = rows.filter(r => r.gitagent.scored && r.terminus.scored);
  const tok = r => (r.gitagent.input_tokens || 0) + (r.gitagent.output_tokens || 0);
  const ttok = r => (r.terminus.input_tokens || 0) + (r.terminus.output_tokens || 0);
  const sorters = { task: (a,b) => a.task.localeCompare(b.task), tokens: (a,b) => tok(b) - tok(a),
    delta: (a,b) => (tok(b) - ttok(b)) - (tok(a) - ttok(a)), cost: (a,b) => (b.gitagent.cost_per_run||0) - (a.gitagent.cost_per_run||0),
    result: (a,b) => ((b.gitagent.pass_rate??0) - (b.terminus.pass_rate??0)) - ((a.gitagent.pass_rate??0) - (a.terminus.pass_rate??0)) };
  rows.sort(sorters[$("#c-sort").value] || sorters.task);
  $("#cmp-n").textContent = `${rows.length} task(s) · ${ml} only`;
  const res = s => !s.scored ? '<span class="muted">—</span>' : `${s.passed}/${s.scored} ${bar(s.passed, s.scored)}`;
  const unp = !M(S.cmpModel).priced;
  const side = s => !s.scored ? '<td class="muted" colspan="6">not run</td>' :
    `<td class="num">${res(s)}</td><td class="num">${fmtN(s.input_tokens)}</td><td class="num muted">${fmtN(s.cache_tokens)}</td><td class="num">${fmtN(s.output_tokens)}</td><td class="num">${s.mean_turns?.toFixed(0) ?? "—"} · ${fmtS(s.median_agent_s)}</td><td class="num">${unp ? NA : usd(s.cost_per_run)}</td>`;
  const sideHead = `<th class="num">Result</th><th class="num">In</th><th class="num">Cache</th><th class="num">Out</th><th class="num">Turns · time</th><th class="num">Cost</th>`;
  $("#cmp-tasks").innerHTML = `<thead><tr><th rowspan="2">Task</th><th colspan="6">gitagent · ${esc(ml)}</th><th colspan="6">Terminus 2 · ${esc(ml)}</th><th rowspan="2" class="num">Tokens vs Terminus</th></tr>
    <tr>${sideHead}${sideHead}</tr></thead><tbody>` +
    (rows.length ? rows.map(r => {
      const delta = r.gitagent.scored && r.terminus.scored && ttok(r) ? (tok(r) / ttok(r) - 1) * 100 : null;
      return `<tr><td><b>${esc(r.task)}</b> ${diff(r.difficulty)}${r.machine_blocked ? ' <span class="badge b-error">machine fails</span>' : ""}</td>${side(r.gitagent)}${side(r.terminus)}
        <td class="num ${delta == null ? "muted" : delta <= 0 ? "ok" : "bad"}">${delta == null ? "—" : (delta > 0 ? "+" : "") + delta.toFixed(0) + "%"}</td></tr>`;
    }).join("") : `<tr><td colspan="14" class="muted">${c.rows.length ? "No task matches this filter." : `No scored run on ${esc(ml)} yet.`}</td></tr>`) + "</tbody>";
}

function roundLessons(name, L, D, commit) {
  const lessonList = (xs, cls) => xs.map(l => `<li class="${cls}">${esc(l)}</li>`).join("");
  if (L == null) return `<p class="muted">${name === "r0" ? "Lessons changed from pass to pass in this round (0 to 9)." : "No lessons recorded for this round."}</p>`;
  return `<p class="muted">${L.length} lessons in the prompt (from <code>prompts/lessons.md</code> at ${esc(commit)})${D ? ` · <span class="ok">${D.added.length} new</span> · <span class="bad">${D.dropped.length} dropped</span> since the previous round (a reworded lesson counts as one of each)` : ""}.</p>
    ${D && D.added.length ? `<h3>New in this round</h3><ul>${lessonList(D.added, "ok")}</ul>` : ""}
    ${D && D.dropped.length ? `<h3>Dropped (kept in the store, may return)</h3><ul>${lessonList(D.dropped, "bad")}</ul>` : ""}
    <details><summary>All ${L.length} lessons this round used</summary><ol>${L.map(l => `<li>${esc(l)}</li>`).join("")}</ol></details>`;
}
function roundTitle(name, commit, host, model) {
  return `<h2>Round ${esc(name)}</h2>${model ? mchip(model, `model: ${model}`) : '<span class="badge">no model</span>'}${commit ? `<span class="muted mono">${esc(commit)}</span>` : ""}${host ? `<span class="badge">${esc(host)}</span>` : ""}`;
}
const resultCard = h => card(pp(h.accuracy), `${kind(h.kind)} ${h.model ? mchip(h.model) : ""} mean pass rate per task`,
  `${h.done}/${h.scope} ${esc(h.scope_label)} done${h.running.length ? ` (${h.running.length} running)` : ""} · ${h.runs} runs · ${h.passed} passed every run · runnable ${pp(h.runnable_accuracy)}
  · timeouts ${h.timeouts} · median ${fmtS(h.median_agent_s)} · cost ${costCell(h.cost_usd, h.unpriced_runs)}${h.per_run.length > 1 ? `<br>Single run: ${h.per_run.map(x => `run ${x.run} ${pp(x.accuracy)} (${x.passed}/${x.tasks})`).join(" · ")}` : ""}${h.failed.length ? `<br>Not always passed: ${h.failed.map(esc).join(", ")}` : ""}${h.model_mismatch ? `<br><span class="bad">bench/rounds.json says this round ran ${esc(M(h.round_model).label)}; these trials ran ${esc(M(h.model).label)} and are kept apart.</span>` : ""}`,
  h.kind === "gitagent" ? "final" : "");

// ---- Harbor's own record of a job (its result.json and config.json) ----
const MULT_LABEL = { timeout_multiplier:"base", agent_timeout_multiplier:"agent", verifier_timeout_multiplier:"verifier",
  agent_setup_timeout_multiplier:"agent setup", environment_build_timeout_multiplier:"env build" };
const okBadge = ok => `<span class="badge ${ok ? "b-pass" : "b-warn"}">${ok ? "ok" : "warn"}</span>`;
const dflt = (v, isDefault) => `${esc(v)}${isDefault ? ' <span class="muted" title="not in config.json, so Harbor used its default">(default)</span>' : ""}`;
const shortHash = (s, n = 12) => s ? `<span class="mono" title="${esc(s)}">${esc(String(s).replace(/^sha256:/, "").slice(0, n))}</span>` : '<span class="muted">—</span>';
const baseName = p => p ? String(p).split("/").pop() : null;
const passK = e => e && Object.keys(e.pass_at_k || {}).length ? Object.entries(e.pass_at_k).map(([k, v]) => `pass@${esc(k)} <b>${pp(v)}</b>`).join(" · ")
  : '<span class="muted" title="Harbor writes pass@k for k = 2, 4, 8… and 5, 10… up to the fewest trials any task has, so none when a task has one trial">none</span>';
const meanGap = e => e && e.differs ? ` <span class="badge b-warn" title="Harbor's mean ${pp(e.mean)} against this dashboard's ${pp(e.dash_accuracy)} (${e.dash_passed}/${e.dash_scored} trials with a reward): more than 0.5 points apart">${e.diff_pts > 0 ? "+" : ""}${e.diff_pts.toFixed(1)} pts</span>` : "";
const hubCell = h => h.hub_url ? `<a href="${esc(h.hub_url)}" target="_blank" rel="noopener">Hub</a>`
  : h.upload_unfinished ? '<span class="badge b-warn" title="a .harbor-upload folder is left in the job directory: an upload started and did not finish">upload unfinished</span>'
  : '<span class="muted" title="no link for this job in bench/hub.json">—</span>';
const kwargsText = a => { const rest = Object.entries(a.kwargs || {}).filter(([k]) => k !== "profile" && k !== "timeout_s");
  return rest.length ? rest.map(([k, v]) => `${esc(k)}=${esc(typeof v === "object" ? JSON.stringify(v) : v)}`).join(", ") : ""; };
const fmtMult = x => typeof x === "number" && Number.isInteger(x) ? x.toFixed(1) : String(x);
// Multipliers as {name: [values seen]} (a job or round) or {name: value} (one trial).
const multText = m => { const vals = Object.entries(m || {}).map(([k, v]) => [k, Array.isArray(v) ? v : [v]]);
  if (!vals.length) return '<span class="muted">not recorded</span>';
  const all = [...new Set(vals.flatMap(([, v]) => v))];
  return all.length === 1 ? `all <b>${esc(fmtMult(all[0]))}</b>` : vals.map(([k, v]) => `${esc(MULT_LABEL[k] || k)} <b>${v.map(x => esc(fmtMult(x))).join("/")}</b>`).join(", "); };

function complianceStrip(c) {
  if (!c) return "";
  const item = (ok, html, title = "") => `<span class="chip"${title ? ` title="${esc(title)}"` : ""}>${okBadge(ok)} ${html}</span>`;
  const overrides = Object.entries(c.timeout_overrides || {});
  return `<div class="chips strip" id="compliance">` + [
    item(c.n_attempts.length === 1, `<b>${c.n_attempts.join(" / ")}</b> trial${c.n_attempts.join() === "1" ? "" : "s"} per task`,
      `n_attempts in each job's config.json${c.n_attempts_defaulted.length ? `; not set (Harbor's default of 1) in ${c.n_attempts_defaulted.join(", ")}` : ""}${c.n_attempts.length > 1 ? "; the round's jobs differ" : ""}`),
    item(c.multipliers_ok, `timeout multipliers ${multText(c.multipliers)}${overrides.length ? ` · overrides ${overrides.map(([k, v]) => `${esc(k)} <b>${v.map(esc).join("/")}</b>`).join(", ")}` : ""}`,
      `As Harbor applied them to every trial (an unset per-phase multiplier uses the base one): ${Object.entries(c.multipliers || {}).map(([k, v]) => `${MULT_LABEL[k] || k} ${v.map(fmtMult).join("/")}`).join(", ") || "none recorded"}`),
    item(c.retries === 0, `retries <b>${c.retries}</b>`, "n_retries in the jobs' result.json, summed"),
    item(c.errored === 0 && c.cancelled === 0, `errored <b>${c.errored}</b>${c.cancelled ? ` · cancelled <b>${c.cancelled}</b>` : ""}`, "n_errored_trials and n_cancelled_trials in the jobs' result.json, summed"),
    item(c.present === c.expected && c.expected_exact, `trials present <b>${c.present}</b> of expected <b>${c.expected}</b>`,
      `Expected: each job's tasks × n_attempts × agents, summed over ${c.jobs} job(s)${c.expected_exact ? "" : " (a job's config.json does not list its tasks, so the tasks seen stand in for them)"}. Harbor's own total: ${c.harbor_total}.`),
    item(c.max_versions === 1 && !c.multi_version_tasks.length && !c.unversioned_tasks.length,
      `task versions <b>${c.max_versions}</b> checksum${c.max_versions === 1 ? "" : "s"} per task${c.multi_version_tasks.length ? ` · ${c.multi_version_tasks.length} task(s) with several: ${c.multi_version_tasks.map(esc).join(", ")}` : ""}${c.unversioned_tasks.length ? ` · ${c.unversioned_tasks.length} without one yet` : ""}`,
      `Distinct task_checksum values per task over the round's ${c.tasks} task(s); 1 means every trial of a task ran the same task version.`),
    ...(c.unfinished_jobs.length ? [item(false, `${c.unfinished_jobs.length} job(s) not finished: ${c.unfinished_jobs.map(esc).join(", ")}`, "no job-level result.json yet")] : []),
  ].join("") + `</div>`;
}

function harborTable(jobs, id) {
  const rows = jobs.flatMap(j => { const h = j.harbor || {}; return (h.evals && h.evals.length ? h.evals : [null]).map(e => ({ j, h, e })); });
  const cnt = v => v == null ? '<span class="muted">—</span>' : v;
  return `<table id="${id}" class="sortable"><thead><tr><th>Job</th><th>Harbor mean</th><th>Dashboard</th><th>pass@k</th><th>Completed</th><th>Errored</th><th>Retries</th><th>Cancelled</th><th>Pending</th>
    <th>Trials / task</th><th>Concurrency</th><th>Agent</th><th>Model</th><th>Profile</th><th>timeout_s</th><th>Dataset</th><th>Started</th><th>Finished</th><th>Hub</th></tr></thead><tbody>` +
    (rows.length ? rows.map(({ j, h, e }) => `<tr class="click" data-jobview="${esc(j.name)}" title="Open the job's trials and Harbor record">
      <td><b>${esc(j.name)}</b>${h.finished ? "" : ' <span class="badge b-running pulse">running</span>'}</td>
      <td class="num"><b>${e ? pp(e.mean) : "—"}</b>${meanGap(e)}</td>
      <td class="num">${e ? `${pp(e.dash_accuracy)} <span class="muted">(${e.dash_passed}/${e.dash_scored})</span>` : "—"}</td>
      <td>${passK(e)}</td>
      <td class="num">${cnt(h.completed)}${h.n_total_trials != null ? `<span class="muted">/${h.n_total_trials}</span>` : ""}</td>
      <td class="num ${h.errored ? "bad" : ""}">${cnt(h.errored)}</td><td class="num">${cnt(h.retries)}</td><td class="num">${cnt(h.cancelled)}</td><td class="num">${cnt(h.pending)}</td>
      <td class="num">${dflt(h.n_attempts, h.n_attempts_default)}</td><td class="num">${dflt(h.n_concurrent_trials, h.n_concurrent_default)}</td>
      <td class="mono">${h.agents.map(a => dflt(a.name, h.agents_default)).join("<br>")}</td>
      <td>${h.agents.map(a => a.model_id ? `${mchip(a.model_id, a.model_name)} <span class="muted mono">${esc(a.model_name)}</span>` : '<span class="muted">—</span>').join("<br>")}</td>
      <td>${h.agents.map(a => a.profile ? `<span class="mono" title="${esc(a.profile)}">${esc(baseName(a.profile))}</span>` : '<span class="muted">—</span>').join("<br>")}${h.agents.some(kwargsText) ? `<br><span class="muted mono">${h.agents.map(kwargsText).join("; ")}</span>` : ""}</td>
      <td class="num">${h.agents.map(a => a.timeout_s ?? "—").join("<br>")}</td>
      <td>${h.datasets.map(ds => `${esc(ds.name)} ${shortHash(ds.ref, 8)} <span class="muted">${ds.n_tasks != null ? ds.n_tasks + " tasks" : "all tasks"}</span>`).join("<br>") || '<span class="muted">—</span>'}</td>
      <td class="dt muted">${when(h.started_at)}</td><td class="dt muted">${when(h.finished_at)}</td><td>${hubCell(h)}</td></tr>`).join("")
    : `<tr><td colspan="19" class="muted">No Harbor job matched.</td></tr>`) + "</tbody></table>";
}

function harborJobView(j) {
  const h = j.harbor; if (!h) return "";
  const cards = (h.evals || []).map(e => card(`${pp(e.mean)}${meanGap(e)}`, `Harbor mean reward ${kind(e.agent)} ${e.model ? mchip(e.model) : ""}`,
    `Harbor's own figure from the job's result.json, over ${e.n_trials ?? "?"} trial(s) of ${esc(e.dataset)}, ${e.n_errors ?? 0} errored${Object.keys(e.exceptions || {}).length ? ` (${Object.entries(e.exceptions).map(([k, v]) => `${esc(k)} ${v}`).join(", ")})` : ""}. This dashboard: ${pp(e.dash_accuracy)} (${e.dash_passed}/${e.dash_scored} trials with a reward)${e.differs ? `, more than 0.5 points apart, usually because Harbor's mean also counts trials that ended without a reward (errored or cancelled) as 0.` : ", the same within 0.5 points."}`, "final")
    + card(passK(e), "pass@k (Harbor)", "Harbor writes pass@k for k = 2, 4, 8… and 5, 10… up to the fewest trials any task has in the job, so a job with one trial per task has none.")).join("");
  const counts = card(h.finished ? `${h.completed}/${h.n_total_trials}` : "—", "Trials completed (Harbor)",
    h.finished ? `errored ${h.errored} · retries ${h.retries} · cancelled ${h.cancelled} · pending ${h.pending} · running ${h.running}. ${h.present_trials} trial folder(s) here, ${h.expected_trials} expected (${h.n_tasks} task(s) × ${h.n_attempts} × ${h.agents.length} agent(s)).`
      : "The job has not written its result.json yet, so Harbor's counts are not in.");
  const kv = [
    ["job id", h.id ? `<span class="mono">${esc(h.id)}</span>` : "—"],
    ["agent", h.agents.map(a => `<span class="mono">${dflt(a.name, h.agents_default)}</span>`).join(", ")],
    ["model", h.agents.map(a => a.model_id ? `${mchip(a.model_id, a.model_name)} <span class="mono">${esc(a.model_name)}</span>` : "—").join(", ")],
    ["agent kwargs", h.agents.map(a => Object.keys(a.kwargs || {}).length ? Object.entries(a.kwargs).map(([k, v]) => `<span class="mono">${esc(k)}=${esc(typeof v === "object" ? JSON.stringify(v) : v)}</span>`).join("<br>") : '<span class="muted">none</span>').join("<br>")],
    ["trials per task", dflt(h.n_attempts, h.n_attempts_default)],
    ["concurrency", dflt(h.n_concurrent_trials, h.n_concurrent_default)],
    ["dataset", h.datasets.map(ds => `${esc(ds.name)} ${shortHash(ds.ref, 16)} <span class="muted">${ds.n_tasks != null ? ds.n_tasks + " tasks listed" : "all tasks"}</span>`).join("<br>") || "—"],
    ["timeout multipliers", `${multText(h.multipliers)}${Object.keys(h.timeout_overrides || {}).length ? ` · overrides ${Object.entries(h.timeout_overrides).map(([k, v]) => `${esc(k)} ${v.map(esc).join("/")}`).join(", ")}` : ""}`],
    ["task versions", `${h.task_versions} checksum(s) per task ${okBadge(h.task_versions <= 1)}`],
    ["started / finished", `<span class="dt">${when(h.started_at)}</span> → <span class="dt">${when(h.finished_at)}</span> <span class="muted">(Harbor's job clock)</span>`],
    ["Harbor Hub", h.hub_url ? `<a href="${esc(h.hub_url)}" target="_blank" rel="noopener">${esc(h.hub_url)}</a>` : `${hubCell(h)} <span class="muted">add the job to bench/hub.json once it is uploaded</span>`],
  ].map(([k, v]) => `<span class="k">${k}</span><span>${v}</span>`).join("");
  return `<div class="panel"><div class="titlerow"><h2>Harbor: job ${esc(j.name)}</h2>${j.round ? `<button class="btn" data-round="${esc(j.round)}">Round ${esc(j.round)}</button>` : ""}<span class="spacer"></span><button class="btn" id="clear-job">Show all jobs</button></div>
    <p class="muted">What Harbor itself recorded for this job in its result.json and config.json, next to this dashboard's own numbers for the same trials. Settings Harbor left at their default are marked.</p>
    <div class="cols">${cards}${counts}</div><h3>Configuration</h3><div class="kv">${kv}</div></div>`;
}

function renderRounds(d) {
  const models = modelList(d), ourFull = new Set(models.map(m => m.full));
  // Our rows carry the model their trials ran (from the trials, not the label), one row per model.
  const mine = d.rounds_page.flatMap(r => r.results.filter(h => h.kind !== "oracle").map(h => ({
    ours: true, round: r.name, model_id: h.model, harness: h.kind === "terminus" ? "Terminus 2 (run here)" : `gitagent, round ${r.name}${r.split === "held-out" ? " (held-out)" : ""}`, model: M(h.model).full,
    accuracy: h.accuracy == null ? null : 100 * h.accuracy, scope: `${h.done} ${h.scope_label}${h.runs > h.done ? `, ${(h.runs / Math.max(h.done,1)).toFixed(0)} runs each` : ""}${h.running.length ? ", still running" : ""}`,
    source: "this machine", date: (h.last || h.first || "").slice(0, 10) })));
  const pub = (d.leaderboard.entries || []).map(e => ({ ...e, scope: "all 89 tasks" }));
  const rows = [...mine, ...pub].filter(r => r.accuracy != null).sort((a, b) => b.accuracy - a.accuracy);
  $("#lb-table").innerHTML = `<thead><tr><th>#</th><th>Harness</th><th>Model</th><th>Accuracy</th><th></th><th>Tasks</th><th>Source</th><th>Date</th></tr></thead><tbody>` +
    (rows.length ? rows.map((r, i) => `<tr class="${r.ours ? "sel click" : ""}" ${r.ours ? `data-round="${esc(r.round)}" title="Open round ${esc(r.round)}"` : ""}><td class="num muted">${i + 1}</td><td>${r.ours ? "<b>" + esc(r.harness) + "</b>" : esc(r.harness)}</td>
      <td>${r.ours ? `${mchip(r.model_id)} ${esc(r.model)}` : esc(r.model)}${!r.ours && ourFull.has(r.model) ? ' <span class="badge b-running">same model as ours</span>' : ""}</td>
      <td class="num"><b>${r.accuracy.toFixed(1)}%</b>${r.error ? ` <span class="muted">±${r.error}</span>` : ""}</td><td>${bar(r.accuracy, 100, r.ours ? "b-acc" : "")}</td>
      <td class="muted">${esc(r.scope)}</td><td>${r.url ? `<a href="${esc(r.url)}" target="_blank" rel="noopener">${esc(r.source)}</a>` : esc(r.source)}</td><td class="muted dt">${esc(r.date || "")}</td></tr>`).join("")
    : `<tr><td colspan="8" class="muted">No scored rounds and no leaderboard entries yet.</td></tr>`) + "</tbody>";

  // Round cards grouped by model (newest round first inside a group); model-less rounds (oracle) last.
  const groups = [...models.map(m => ({ id: m.id, title: m.full, rounds: d.rounds_page.filter(r => r.model === m.id) })),
    { id: null, title: "Machine checks (oracle, no model)", rounds: d.rounds_page.filter(r => !r.model) }].filter(g => g.rounds.length);
  const roundCard = r => `<div class="panel">
      <div class="titlerow">${roundTitle(r.name, r.commit, r.host, r.model)}${r.split === "held-out" ? '<span class="badge">held-out</span>' : r.split === "all" ? '<span class="badge">all 89 tasks</span>' : ""}<span class="spacer"></span><button class="btn" data-round="${esc(r.name)}">Open round</button></div>
      <p>${esc(r.label || "")}</p>
      <div class="cols">${r.results.map(resultCard).join("") || empty("No scored runs yet.")}</div>
      <h3>What was done</h3><ul>${(r.changes || []).map(c => `<li>${esc(c)}</li>`).join("") || '<li class="muted">—</li>'}</ul>
      <h3>Why it scored as it did</h3><p>${esc(r.reasoning || "—")}</p>
      <h3>Lessons</h3>${roundLessons(r.name, r.lessons, r.lessons_diff, r.lessons_commit)}</div>`;
  $("#round-cards").innerHTML = groups.map(g => { const cur = g.id ? ((d.progress.by_model[g.id] || {}).gitagent || {}).round : null;
    return `<div class="grouphead">${g.id ? mchip(g.id) : ""}<h2>${esc(g.title)}</h2><span class="muted">${g.rounds.length} round(s)${cur ? ` · current ${esc(cur.name)}` : ""}</span></div>` +
      g.rounds.slice().reverse().map(roundCard).join(""); }).join("") || empty("No rounds in <code>bench/rounds.json</code> yet.");
}

function renderRound(d) {
  const el = $("#round-detail");
  if (!S.round) { el.innerHTML = ""; return; }
  const name = S.round;
  const info = (d.rounds_page || []).find(r => r.name === name) || { name, results: [] };
  const rows = d.progress.history.filter(h => h.round === name);
  const allTr = d.jobs.filter(j => j.round === name).flatMap(j => j.trials.map(t => ({ ...t, _job: j.name })));
  // Only trials of the round's declared model go into its tables; others are counted and named, never mixed in.
  const trials = info.model ? allTr.filter(t => t.kind === "oracle" || t.model_id === info.model) : allTr;
  const strayN = allTr.length - trials.length;
  const head = `<div class="pagehead"><button class="btn" id="close-round">← Back</button><span class="crumbs">bench/rounds.json / ${esc(name)}</span></div>`;
  if (!rows.length && !trials.length) { el.innerHTML = `<div class="panel">${head}<div class="titlerow">${roundTitle(name, info.commit, info.host, info.model)}</div>${empty("No runs in this round yet.")}</div>`; return; }
  const sum = (xs, f) => xs.reduce((a, t) => a + (f(t) || 0), 0);
  const meta = Object.fromEntries((d.tasks || []).map(t => [t.task, t]));

  // Compared with the round before it: same harness, same split, same model.
  const order = d.progress.history;
  const prevOf = h => { const i = order.indexOf(h); for (let j = i - 1; j >= 0; j--) { const o = order[j];
    if (o.kind === h.kind && o.scope_label === h.scope_label && o.model === h.model && o.round !== h.round) return o; } return null; };

  // Published results, for this round's place among them.
  const pub = (d.leaderboard.entries || []).filter(e => e.accuracy != null);

  const cards = rows.map(h => {
    const prev = prevOf(h);
    const delta = prev && h.accuracy != null && prev.accuracy != null ? 100 * (h.accuracy - prev.accuracy) : null;
    const mine = allTr.filter(t => t.kind === h.kind && (t.model_id || null) === (h.model || null));
    const inp = sum(mine, t => t.input_tokens), cache = sum(mine, t => t.cache_tokens), out = sum(mine, t => t.output_tokens);
    const turns = mine.filter(t => t.turns); const meanTurns = turns.length ? sum(turns, t => t.turns) / turns.length : null;
    const above = h.accuracy != null && h.kind !== "oracle" ? pub.filter(e => e.accuracy < 100 * h.accuracy).length : null;
    return `<div class="cols">
      ${card(pp(h.accuracy), `${kind(h.kind)} ${h.model ? mchip(h.model) : ""} mean pass rate per task`, `Each task's pass rate over its ${h.runs} run(s) on ${esc(h.model ? M(h.model).full : "the reference solution")}, averaged over ${h.done} ${esc(h.scope_label)}. ${h.per_run.length > 1 ? "Single runs: " + h.per_run.map(x => `run ${x.run} ${pp(x.accuracy)} (${x.passed}/${x.tasks})`).join(", ") + "." : ""}${h.model_mismatch ? ` <span class="bad">bench/rounds.json says this round ran ${esc(M(h.round_model).label)}; these trials ran ${esc(M(h.model).label)}.</span>` : ""}`, "final")}
      ${card(pp(h.runnable_accuracy), "Runnable accuracy", `Leaving out tasks this machine cannot run (nobody, the reference solution included, has passed them here): ${h.runnable_done} tasks.`)}
      ${card(`${h.done}/${h.scope}`, "Tasks done", `${h.passed} passed every run, ${h.failed.length} failed at least once${h.running.length ? `, ${h.running.length} running` : ""}${h.pending.length ? `, ${h.pending.length} not started` : ""}.`)}
      ${delta == null ? card("—", "Change from the previous round", `No earlier round of this harness on these tasks with ${esc(h.model ? M(h.model).full : "no model")}.`) :
        card(`<span class="${delta >= 0 ? "ok" : "bad"}">${delta >= 0 ? "+" : ""}${delta.toFixed(1)} pts</span>`, `Against round ${esc(prev.round)} ${h.model ? mchip(h.model) : ""}`, `${pp(prev.accuracy)} → ${pp(h.accuracy)}, same harness and model. With few runs per task, a point or two either way is noise.`)}
      ${above == null ? "" : card(`${above} ${sm(`of ${pub.length}`)}`, "Published results it is above", `Among the Terminal-Bench 2.1 results in bench/leaderboard.json (all 89 tasks, several models). This round covers ${h.done} ${esc(h.scope_label)} on ${esc(M(h.model).full)}, so the comparison is indicative.`)}
      ${card(costCell(h.cost_usd, h.unpriced_runs), "Cost", `${h.unpriced_runs ? NA : usd(h.cost_per_run)} per run · ${h.timeouts} timeout(s) · median agent time ${fmtS(h.median_agent_s)}${meanTurns != null ? ` · ${meanTurns.toFixed(1)} turns per run` : ""}.`)}
      ${inp ? card(fmtN(inp), "Input tokens", `${pct(cache, inp)} from the cache (${fmtN(cache)}) · ${fmtN(out)} output tokens.`) : ""}
    </div>`;
  }).join("");

  // Per task: every attempt in start order, linked to its trial page.
  const byTask = {};
  for (const t of trials.slice().sort((a, b) => (a.started || "").localeCompare(b.started || ""))) (byTask[t.task] = byTask[t.task] || []).push(t);
  const prevRow = rows[0] ? prevOf(rows[0]) : null;
  const prevTrials = prevRow ? d.jobs.filter(j => j.round === prevRow.round).flatMap(j => j.trials).filter(t => t.kind === rows[0].kind && (t.model_id || null) === (rows[0].model || null)) : [];
  const prevRate = task => { const xs = prevTrials.filter(t => t.task === task && t.reward != null); return xs.length ? xs.filter(t => t.reward >= 1).length / xs.length : null; };
  const rate = xs => { const sc = xs.filter(t => t.reward != null); return sc.length ? sc.filter(t => t.reward >= 1).length / sc.length : null; };
  const tasks = Object.entries(byTask).map(([task, xs]) => ({ task, xs, r: rate(xs), p: prevRate(task) }))
    .sort((a, b) => (a.r ?? 2) - (b.r ?? 2) || a.task.localeCompare(b.task));
  const mark = t => `<a href="#" data-open="${esc(t._job)}|${esc(t.trial)}" title="${esc(t.status)}">${badge(t.status)}</a>`;
  const taskTable = `<table id="round-tasks" class="sortable"><thead><tr><th>Task</th><th>Difficulty</th><th>Attempts (in order; click to open)</th><th>Pass rate</th>${prevRow ? `<th>In ${esc(prevRow.round)}</th>` : ""}<th>Turns</th><th>Median time</th><th>Cost</th></tr></thead><tbody>` +
    (tasks.length ? tasks.map(({ task, xs, r, p }) => {
      const tt = xs.filter(t => t.turns), times = xs.map(t => t.agent_s).filter(x => x != null).sort((a, b) => a - b);
      const changed = p != null && r != null && (r >= 1) !== (p >= 1);
      return `<tr><td><b>${esc(task)}</b>${meta[task] && meta[task].machine_blocked ? ' <span class="badge b-error">machine fails</span>' : ""}</td>
        <td>${meta[task] ? diff(meta[task].difficulty) : "—"}</td><td>${xs.map(mark).join(" ")}</td>
        <td class="num ${r == null ? "muted" : r >= 1 ? "ok" : "bad"}">${pp(r)}</td>
        ${prevRow ? `<td class="num ${changed ? (r > p ? "ok" : "bad") : "muted"}">${pp(p)}</td>` : ""}
        <td class="num">${tt.length ? (sum(tt, t => t.turns) / tt.length).toFixed(0) : "—"}</td>
        <td class="num">${times.length ? fmtS(Math.round(times[Math.floor(times.length / 2)])) : "—"}</td><td class="num">${xs.some(t => t.cost_usd == null && (t.input_tokens || 0) > 0) ? NA : usd(sum(xs, t => t.cost_usd))}</td></tr>`;
    }).join("") : `<tr><td colspan="8" class="muted">No trials matched to this round yet.</td></tr>`) + "</tbody></table>";

  const jobs = [...new Set(allTr.map(t => t._job))].sort();
  const roundJobs = d.jobs.filter(j => j.round === name).sort((a, b) => a.name.localeCompare(b.name));
  el.innerHTML = `
    <div class="panel">${head}
      <div class="titlerow">${roundTitle(name, info.commit, info.host, info.model)}${info.split === "held-out" ? '<span class="badge">held-out</span>' : info.split === "all" ? '<span class="badge">all 89 tasks</span>' : ""}${rows.some(h => h.current) ? '<span class="badge b-running">current</span>' : ""}${strayN ? `<span class="badge b-warn" title="trials in this round's jobs that ran another model; they are listed under Trials & logs and never counted here">${strayN} trial(s) on another model</span>` : ""}</div>
      ${complianceStrip(info.compliance)}
      <p>${esc(info.label || "")}</p>${cards}</div>
    <div class="panel"><h2>Harbor</h2>
      <p class="muted">What Harbor itself recorded for each of the round's ${jobs.length} job(s) (matched by bench/rounds.json): its mean reward and pass@k from the job's result.json, beside this dashboard's accuracy over the same trials (a warn badge marks a gap over 0.5 points); its trial counts; and the settings from the job's config.json. Settings Harbor left at their default are marked. The strip above sums them for the round. Click a job for its trials.</p>
      <div class="scroll">${harborTable(roundJobs, "round-harbor")}</div></div>
    <div class="panel"><h2>What was done</h2><ul>${(info.changes || []).map(c => `<li>${esc(c)}</li>`).join("") || '<li class="muted">—</li>'}</ul>
      <h2 style="margin-top:12px">Why it scored as it did</h2><p>${esc(info.reasoning || "—")}</p></div>
    <div class="panel"><div class="titlerow"><h2>Per task</h2>${info.model ? mchip(info.model) : ""}</div><p class="muted">Lowest pass rate first${info.model ? `, ${esc(M(info.model).full)} runs only` : ""}. Each badge is one attempt; click it for the trial's log and trajectory.${prevRow ? ` The "In ${esc(prevRow.round)}" column is the same task's pass rate in the previous ${esc(M(prevRow.model).label)} round of this harness; green or red marks a task that flipped.` : ""}</p>
      <div class="scroll">${taskTable}</div></div>
    <div class="panel"><h2>Lessons</h2>${roundLessons(name, info.lessons, info.lessons_diff, info.lessons_commit)}</div>`;
}

function renderTrials(d) {
  const sel = $("#f-job");
  sel.innerHTML = `<option value="">all</option>` + d.jobs.map(j => `<option ${j.name===S.job?"selected":""}>${esc(j.name)}</option>`).join("");
  const msel = $("#f-model");
  msel.innerHTML = `<option value="">all</option>` + modelList(d).map(m => `<option value="${esc(m.id)}" ${m.id===S.model?"selected":""}>${esc(m.label)}</option>`).join("");
  const job = S.job ? d.jobs.find(j => j.name === S.job) : null;
  $("#job-harbor").innerHTML = job ? harborJobView(job) : "";
  let rows = allTrials(d).filter(t => (!S.job || t._job===S.job) && (!S.kind || t.kind===S.kind) && (!S.status || t.status===S.status) && inModel(t));
  rows.sort((a,b) => (b.started||"").localeCompare(a.started||""));
  $("#trials-n").textContent = `${rows.length} trial(s)${S.model ? ` · ${M(S.model).label} only` : ""}`;
  $("#trials tbody").innerHTML = rows.length ? rows.map(r => `
    <tr class="click ${S.trial===r.trial && S.trialJob===r._job ?"sel":""}" data-open="${esc(r._job)}|${esc(r.trial)}">
      <td class="mono muted">${esc(r.trial)}</td><td><b>${esc(r.task)}</b></td><td>${kind(r.kind)}</td><td>${r.kind === "oracle" ? '<span class="muted">—</span>' : mchip(r.model_id, r.model)}</td><td>${badge(r.status)}</td>
      <td>${r.outcome != null ? outc(r.outcome) : r.exception ? `<span class="badge b-error" title="${esc(r.exception)}">${esc(r.exception)}</span>` : '<span class="muted">—</span>'}</td><td class="num">${r.turns ?? "—"}</td>
      <td class="num">${fmtN(r.input_tokens)}</td><td class="num">${fmtN(r.cache_tokens)}</td><td class="num">${fmtN(r.output_tokens)}</td><td class="num">${trialCost(r)}</td>
      <td class="num">${fmtS(r.agent_s)}</td><td class="num">${fmtS(r.verifier_s)}</td><td class="dt muted">${when(r.started)}</td></tr>`).join("")
    : `<tr><td colspan="14" class="muted">No trial matches these filters.</td></tr>`;
}

function renderFailures(d) {
  const all = allTrials(d).filter(t => !["pass","running"].includes(t.status));
  const rows = all.filter(inModel);
  $("#failures-n").textContent = S.model ? `${rows.length} of ${all.length}, ${M(S.model).label} only` : `${rows.length}, all models`;
  $("#failures").innerHTML = rows.length ? rows.map(r => `
    <div class="card failure">
      <div class="head"><b>${esc(r.task)}</b> ${kind(r.kind)} ${r.kind === "oracle" ? "" : mchip(r.model_id, r.model)} ${badge(r.status)}${r.machine_blocked ? ' <span class="badge b-error">machine fails</span>' : ""} <span class="muted">${esc(r._job)} / ${esc(r.trial)} · ${when(r.started)}</span>
        <a href="#" data-open="${esc(r._job)}|${esc(r.trial)}">open</a></div>
      <p><b>Reason:</b> ${esc(r.diagnosis.reason)}</p>
      ${r.exception_message ? `<pre>${esc(r.exception_message)}</pre>` : ""}
      ${r.diagnosis.notes.length ? `<ul class="muted">${r.diagnosis.notes.map(n=>`<li>${esc(n)}</li>`).join("")}</ul>` : ""}
    </div>`).join("") : empty(S.model ? `Every finished ${esc(M(S.model).label)} trial passed.` : "Every finished trial passed.");
}

function renderGuards(d) {
  const g = d.guard_totals;
  const keys = Object.keys(g).filter(k => !["turns","input_tokens","output_tokens","cache_read_tokens","cache_write_tokens","shaped_bytes_in","shaped_bytes_out"].includes(k)).sort();
  $("#guard-totals").innerHTML = keys.map(k => `<span class="chip" title="${esc((GLOSSARY[k]||GLOSSARY[k.replace(/\..*/,".*")]||[""])[0])}">${esc(k)} <b>${fmtN(g[k])}</b></span>`).join("") || `<span class="muted">no agent runs yet</span>`;
  const cols = ["turns","loop_triggers","stop_accepts","stop_rejections","nudges","forced_stops","compactions","masks_applied","truncated_turns","no_tool_turns","repairs","refusals","exec_timeouts","spill_reads","snapshot_ok","protected_writes_blocked","subagent_calls"];
  const rows = allTrials(d).filter(t => t.kind !== "oracle");
  $("#guard-table").innerHTML = `<thead><tr><th>Trial</th><th>Verdict</th>${cols.map(c=>`<th class="num" title="${esc((GLOSSARY[c]||[""])[0])}">${esc(c)}</th>`).join("")}</tr></thead><tbody>` +
    (rows.length ? rows.map(r => `<tr class="click" data-open="${esc(r._job)}|${esc(r.trial)}"><td>${esc(r.task)} <span class="muted mono">${esc(r.trial.split("__")[1]||"")}</span></td><td>${badge(r.status)}</td>${cols.map(c => `<td class="num ${(r.counters||{})[c] ? "" : "muted"}">${(r.counters||{})[c] ?? 0}</td>`).join("")}</tr>`).join("")
    : `<tr><td colspan="${cols.length + 2}" class="muted">No agent runs yet.</td></tr>`) + "</tbody>";
  $("#glossary tbody").innerHTML = Object.entries(GLOSSARY).map(([k,[m,sw]]) => `<tr><td><code>${esc(k)}</code></td><td class="wrap">${esc(m)}</td><td class="muted">${esc(sw)}</td></tr>`).join("");
  $("#outcomes tbody").innerHTML = Object.entries(OUTCOMES).map(([k,m]) => `<tr><td><code>${esc(k)}</code></td><td class="wrap">${esc(m)}</td></tr>`).join("");
}

function renderLearning(d) {
  $("#learning-how").innerHTML = LEARNING_HOW;
  const L = d.lessons || {lessons:[], learned:[]};
  const shown = (d.lessons_md||"").replace(/<!--[\s\S]*?-->/,"").trim();
  $("#learning-status").innerHTML = [
    ["lessons stored", L.lessons.length], ["lessons shown to the agent", shown ? shown.split("\n").filter(l=>l.startsWith("- ")).length : 0],
    ["trials learned from", (L.learned||[]).length], ["dev split", d.split ? `${(d.split.dev||[]).length} dev / ${(d.split.heldout||d.split.held_out||[]).length} held-out` : "not created yet (needs the baseline runs)"],
    ["online learning", "off in --bench; opt-in per project with learning: online"],
  ].map(([k,v]) => `<span class="k">${k}</span><span>${esc(v)}</span>`).join("");
  $("#lessons").innerHTML = L.lessons.length ? L.lessons.map(l => `
    <div class="lesson"><span class="imp">#${l.id} · imp ${l.importance} · +${l.helpful}/−${l.harmful}<br>${(l.tasks||[]).length} task(s)</span>
    <span>${esc(l.text)}<br><span class="note">${(l.tasks||[]).length >= 2 ? '<span class="ok">shown to the agent</span>' : "stored only: needs a second task to support it"} · from ${esc((l.sources||[]).join(", "))}</span></span></div>`).join("") : empty("No lessons stored yet.");
  $("#lessons-md").textContent = shown || "(empty: no lesson has two supporting tasks yet)";
  $("#learner-md").textContent = (d.learner_md||"").replace(/<!--[\s\S]*?-->/,"").trim();
}

function renderTasks(d) {
  const cats = [...new Set(d.tasks.map(t=>t.category))].sort();
  const cs = $("#t-cat"); const cv = cs.value;
  cs.innerHTML = `<option value="">all</option>` + cats.map(c=>`<option ${c===cv?"selected":""}>${esc(c)}</option>`).join("");
  const q = $("#t-search").value.toLowerCase(), df = $("#t-diff").value, cat = cs.value, sp = $("#t-split").value, seen = $("#t-seen").checked;
  const rows = d.tasks.filter(r => (!q || (r.task+" "+(r.tags||[]).join(" ")+" "+(r.description||"")).toLowerCase().includes(q)) && (!df || r.difficulty===df) && (!cat || r.category===cat) && (!sp || r.split===sp) && (!seen || r.agent_attempts || r.oracle_attempts));
  $("#tasks-n").textContent = `${rows.length} of ${d.tasks.length} tasks`;
  $("#tasks tbody").innerHTML = rows.length ? rows.map(r => `
    <tr><td><b>${esc(r.task)}</b>${r.description ? `<details data-key="task:${esc(r.task)}"><summary>description</summary><div class="muted wrap-text">${esc(r.description)}</div></details>`:""}</td>
    <td>${diff(r.difficulty)}</td><td>${esc(r.category)}</td><td class="muted wrap">${esc((r.tags||[]).join(", "))}</td>
    <td class="num">${fmtS(r.agent_timeout_s)}</td><td class="num">${fmtS(r.verifier_timeout_s)}</td>
    <td class="num">${Object.entries(r.per_model || {}).filter(([, s]) => s.agent_attempts).sort(([a], [b]) => M(a).index - M(b).index)
      .map(([m, s]) => `${mchip(m)} ${s.agent_passed}/${s.agent_attempts} ${bar(s.agent_passed, s.agent_attempts)}`).join("<br>") || "—"}</td>
    <td class="num muted">${r.oracle_attempts ? `${r.oracle_passed}/${r.oracle_attempts}` : "—"}${r.machine_blocked ? ' <span class="badge b-error">machine fails</span>' : ""}</td>
    <td class="muted">${esc(r.split)}</td>
    <td>${r.last ? `${badge(r.last.status)} <a href="#" data-open="${esc(r.last.job)}|${esc(r.last.trial)}">open</a>` : '<span class="muted">—</span>'}</td></tr>`).join("")
    : `<tr><td colspan="10" class="muted">No task matches these filters.</td></tr>`;
}

let designRendered = false;
function renderDesign(d) {
  if (designRendered) return;
  designRendered = true;
  $("#plain-glossary tbody").innerHTML = PLAIN.map(([w,m]) => `<tr><td><b>${esc(w)}</b></td><td class="wrap">${esc(m)}</td></tr>`).join("");
  $("#approaches").innerHTML = APPROACHES.map(([t,b]) => `<div class="card"><h3>${esc(t)}</h3><p style="margin:0">${esc(b)}</p></div>`).join("");
  const P = d.profiles;
  $("#switches").innerHTML = `<thead><tr><th>Switch</th>${P.map(p=>`<th>${esc(p.name.replace(".yaml",""))}</th>`).join("")}</tr></thead><tbody>` +
    SWITCH_ROWS.map(([k,label]) => `<tr><td class="wrap"><code>${esc(k)}</code><br><span class="muted">${esc(label)}</span></td>${P.map(p => { const v = p.flags[k]; const cls = v==="true"?"ok":v==="false"?"bad":""; return `<td class="${cls}">${esc(v ?? "—")}</td>`; }).join("")}</tr>`).join("") + "</tbody>";
  $("#profiles-raw").innerHTML = P.map(p => `<details data-key="profile:${esc(p.name)}"><summary>${esc(p.name)}</summary><pre>${esc(p.text)}</pre></details>`).join("") || empty("No profile files found.");
  $("#docs").innerHTML = Object.entries(d.docs).map(([n,t]) => `<details data-key="doc:${esc(n)}"><summary>${esc(n)}</summary><pre>${esc(t||"(missing)")}</pre></details>`).join("") || empty("No documents found.");
}

async function refresh() {
  try {
    S.data = await (await fetch("/api/summary")).json();
    render();
  } catch (e) { $("#meta").textContent = "lost the server. Is dashboard.py still running?"; }
}

async function openTrial(job, trial, quiet) {
  const r = await fetch(`/api/trial?job=${encodeURIComponent(job)}&trial=${encodeURIComponent(trial)}`);
  if (!r.ok) return;
  const first = S.trial !== trial || S.trialJob !== job;
  S.trial = trial; S.trialJob = job; S.tab = "trial";
  S.detail = await r.json();
  if (first) S.sub = S.detail.harbor && S.detail.harbor.exception ? "harbor" : (S.detail.steps.length ? "turns" : "harbor");
  const top = window.scrollY;
  const hash = `#trial=${encodeURIComponent(job)}/${encodeURIComponent(trial)}`;
  if (location.hash !== hash) history.pushState(null, "", hash);
  renderDetail();
  window.scrollTo(0, quiet ? top : 0);
  clearInterval(S.timer);
  if (S.detail.row.status === "running") S.timer = setInterval(() => openTrial(job, trial, true), 4000);
  render();
}

function renderDetail() {
  const d = S.detail, row = d.row;
  const h = d.harbor || {};
  const subs = [["harbor", h.exception ? "Harbor (error)" : "Harbor"],["turns","Turns"],["trajectory",`Trajectory (${d.steps.length})`],["events",`Guard events (${d.events.length})`],["counters","Counters"],["files",`Commands & files (${d.files.length})`],["log","gitagent.log"]];
  let body = "";
  if (S.sub === "harbor") body = `
    <p class="muted">What Harbor (the benchmark runner) recorded about this trial: its exception if the trial errored, and gitagent's exit code. Harbor's per-trial log is not shown: it can contain the checker's commands and output, which we never read. gitagent's own records are in the other views${h.has_trajectory ? "" : " (none here: gitagent never wrote a trajectory, so it did not start or this is not a gitagent run)"}.</p>
    ${h.exception ? `<h3 class="bad">${esc(h.exception.exception_type || "error")}</h3><p>${esc(h.exception.exception_message || "")} <span class="muted">${esc((h.exception.occurred_at||"").slice(0,19).replace("T"," "))}</span></p>
      ${h.exception.exception_traceback ? `<details open><summary>traceback</summary><pre>${esc(h.exception.exception_traceback)}</pre></details>` : ""}` : `<p class="ok">No Harbor exception.</p>`}
    <h3>gitagent exit code</h3><p>${h.done === "" ? '<span class="muted">no done file: gitagent did not finish on its own (killed, timed out, or never started)</span>' : `<code>${esc(h.done)}</code> ${h.done==="0"?"(normal end)":h.done==="3"?"(the model API failed)":h.done==="2"?"(usage or setup error)":""}`}</p>`;
  else if (S.sub === "turns") body = d.turns.length ? `<div class="scroll"><table class="sortable" id="turns-table"><thead><tr><th>Step</th><th>Tools called</th><th>Prompt tok</th><th>Out tok</th><th>Since prev</th></tr></thead><tbody>` +
    d.turns.map(t => `<tr><td class="num">${t.step}</td><td>${esc(t.tools.join(", ")||"(text only)")}</td><td class="num">${fmtN(t.prompt_tokens)}</td><td class="num">${fmtN(t.completion_tokens)}</td><td class="num">${fmtS(t.since_prev_s)}</td></tr>`).join("") + "</tbody></table></div>"
    : empty("No model turns recorded for this trial.");
  else if (S.sub === "trajectory") body = d.steps.length ? d.steps.map(s => `
      <div class="step ${s.kind?"note":""}"><div class="meta"><span class="src src-${esc(s.source)}">${esc(s.source)} · step ${s.id}</span>
      ${s.kind ? `<span class="imp">${esc(EVENT_NAMES[s.kind]||s.kind)}</span>`:""}${s.compaction ? `<span class="imp">compaction summary</span>`:""}
      ${s.metrics ? `<span class="muted">${fmtN(s.metrics.prompt_tokens)} in, ${fmtN(s.metrics.completion_tokens)} out</span>`:""}
      <span class="muted dt">${esc((s.timestamp||"").slice(11,19))}</span></div>
      ${s.message ? (s.source==="system" ? `<details><summary>system prompt (${s.message.length} chars)</summary><pre>${esc(s.message)}</pre></details>` : `<pre>${esc(s.message)}</pre>`) : ""}
      ${s.calls.map(c=>`<div class="call">→ ${esc(c.name)}(${esc(c.arguments)})</div>`).join("")}
      ${s.observations.map(o=>`<details><summary>output (${o.length} chars)</summary><pre>${esc(o)}</pre></details>`).join("")}
      </div>`).join("") : empty("No trajectory: gitagent never wrote one for this trial.");
  else if (S.sub === "events") body = d.events.length ? d.events.map(e => `<div class="ev"><span class="num muted">step ${e.step}</span><span class="tag">${esc(EVENT_NAMES[e.kind]||e.kind)}</span><span>${esc(e.text)}</span></div>`).join("") : empty("No guard fired in this run: no loops, no rejected task_complete, no time reminders, no compaction.");
  else if (S.sub === "counters") body = `<pre>${esc(JSON.stringify(d.counters, null, 1) ?? "—")}</pre>`;
  else if (S.sub === "files") body = `<p class="muted">commands/ holds every shell command as a script; out/ holds the full output of each command (spill files, numbered in order); gitagent.log is the binary's own log.</p>` +
    (d.files.map(f => `<details data-file="${esc(f.path)}"><summary><span class="mono">${esc(f.path)}</span> <span class="muted">(${fmtN(f.bytes)} B)</span></summary><pre class="muted">loading…</pre></details>`).join("") || empty("No command or output files in this trial."));
  else if (S.sub === "log") body = `<pre>${esc(d.log_tail || (h.has_log ? "(empty)" : "no gitagent.log: gitagent never started in this trial"))}</pre>`;
  $("#detail").innerHTML = `
    <section class="panel">
      <div class="pagehead"><button class="btn" id="close">← Back to trials</button>
        <span class="crumbs">${esc(S.trialJob)} / <span class="mono">${esc(row.trial)}</span></span><span class="spacer"></span>
        ${row.status==="running" ? `<span class="badge b-running pulse">live, refreshing every 4s</span>` : ""}</div>
      <div class="titlerow"><h2>${esc(row.task)}</h2>${badge(row.status)}${kind(row.kind)}${row.kind === "oracle" ? "" : mchip(row.model_id, row.model)}${row.outcome != null || d.outcome != null ? outc(d.outcome ?? row.outcome) : ""}</div>
      <div class="cols">
        <div class="kv">
          <span class="k">task</span><span>${esc(row.task)}</span>
          <span class="k">reward</span><span>${row.reward ?? "—"}</span>
          <span class="k">outcome</span><span>${esc(d.outcome ?? row.outcome ?? "—")}</span>
          <span class="k">model / profile</span><span>${row.model_id ? `${mchip(row.model_id)} ${esc(row.model_id)}${row.provider ? ` <span class="muted">(${esc(row.provider)}, configured as ${esc(row.model)})</span>` : ""}` : "—"} / ${esc(row.profile ?? "—")}</span>
          <span class="k">turns / tokens</span><span>${row.turns ?? "—"} · in ${fmtN(row.input_tokens)} (cached ${fmtN(row.cache_tokens)}) · out ${fmtN(row.output_tokens)}</span>
          <span class="k">est. cost</span><span>${trialCost(row)}${row.model_id && !M(row.model_id).priced && (row.input_tokens || 0) > 0 ? ` <span class="muted">(${esc(row.model_id)} has no price in bench/pricing.json)</span>` : ""}</span>
          <span class="k">task checksum</span><span>${shortHash(h.task_checksum)}</span>
          <span class="k">task ref</span><span>${h.task_id ? `${esc([h.task_id.org, h.task_id.name].filter(Boolean).join("/"))} ${shortHash(h.task_id.ref, 16)}` : '<span class="muted">—</span>'}${h.source ? ` <span class="muted">from ${esc(h.source)}</span>` : ""}</span>
          <span class="k">verifier env</span><span>${h.verifier_environment_mode ? esc(h.verifier_environment_mode) : '<span class="muted">—</span>'}</span>
          <span class="k">timeout multipliers</span><span>${multText(h.effective_multipliers)}${h.timeout_multipliers && Object.entries(h.timeout_multipliers).some(([k, v]) => k !== "timeout_multiplier" && v == null) ? ' <span class="muted">(per-phase ones unset use the base one)</span>' : ""}${Object.keys(h.timeout_overrides || {}).length ? ` · overrides ${Object.entries(h.timeout_overrides).map(([k, v]) => `${esc(k)} ${esc(v)}`).join(", ")}` : ""}</span>
          ${d.timings.map(t=>`<span class="k">${esc(t.phase)}</span><span>${fmtS(t.seconds)}</span>`).join("")}
          ${row.status==="running" ? `<span class="k">live</span><span class="pulse">${esc(row.live?.text||"")} · ${fmtS(row.elapsed_s)} elapsed</span>`:""}
        </div>
        <div class="reason"><b>Reason:</b> ${esc(row.diagnosis.reason)}${row.diagnosis.notes.length ? `<ul class="muted">${row.diagnosis.notes.map(n=>`<li>${esc(n)}</li>`).join("")}</ul>`:""}</div>
      </div>
      <div class="subtabs">${subs.map(([id,l]) => `<button class="subtab" data-sub="${id}" aria-selected="${S.sub===id}">${l}</button>`).join("")}</div>
      ${body}
    </section>`;
  postRender();
}

function closeTrial() {
  S.trial = null; S.detail = null; clearInterval(S.timer); $("#detail").innerHTML = "";
  if (S.tab === "trial") S.tab = "trials";
  if (location.hash.startsWith("#trial=")) history.pushState(null, "", location.pathname);
  render();
}
function openFromHash() {
  const r = location.hash.match(/^#round=(.+)$/);
  if (r) { S.round = decodeURIComponent(r[1]); S.tab = "round"; render(); return; }
  const m = location.hash.match(/^#trial=([^/]+)\/(.+)$/);
  if (m) { openTrial(decodeURIComponent(m[1]), decodeURIComponent(m[2])); return; }
  // #tab=overview&model=gpt-6-astra (&job=<name> on the trials tab), in any order.
  let touched = false;
  for (const part of location.hash.replace(/^#/, "").split("&")) {
    const [k, v = ""] = part.split("=");
    if (k === "tab" && TABS.some(([id]) => id === v)) { S.tab = v; touched = true; }
    else if (k === "model") { S.model = decodeURIComponent(v); if (S.model) S.cmpModel = S.model; touched = true; }
    else if (k === "job") { S.job = decodeURIComponent(v); touched = true; }
  }
  if (touched) { render(); return; }
  if (S.trial) closeTrial();
}
function openRound(name) {
  S.round = name; S.tab = "round";
  if (location.hash !== "#round=" + encodeURIComponent(name)) history.pushState(null, "", "#round=" + encodeURIComponent(name));
  render(); window.scrollTo(0, 0);
}
function closeRound() {
  S.round = null; if (S.tab === "round") S.tab = "overview";
  if (location.hash.startsWith("#round=")) history.pushState(null, "", location.pathname);
  render();
}
window.addEventListener("popstate", openFromHash);
async function loadFile(details) {
  if (details.dataset.loaded) return;
  details.dataset.loaded = "1";
  const r = await fetch(`/api/file?job=${encodeURIComponent(S.trialJob)}&trial=${encodeURIComponent(S.trial)}&path=${encodeURIComponent(details.dataset.file)}`);
  details.querySelector("pre").textContent = r.ok ? await r.text() : "could not read";
  details.querySelector("pre").classList.remove("muted");
}

document.addEventListener("click", e => {
  const th = e.target.closest("table.sortable > thead th"); if (th && th.closest("table").tHead.rows.length === 1) {
    const table = th.closest("table"), col = th.cellIndex, cur = S.sorts[table.id];
    S.sorts[table.id] = cur && cur.col === col ? (cur.dir > 0 ? { col, dir: -1 } : null) : { col, dir: 1 };
    if (!S.sorts[table.id]) delete S.sorts[table.id];
    if (table.id === "turns-table") renderDetail(); else render();
    return; }
  const a = e.target.closest("[data-open]"); if (a) { e.preventDefault(); const [j,t] = a.dataset.open.split("|"); openTrial(j,t); return; }
  const tab = e.target.closest(".tab"); if (tab) { S.tab = tab.dataset.tab; if (!["trial","round"].includes(S.tab)) history.replaceState(null, "", hashFor()); render(); return; }
  const mf = e.target.closest("[data-model]"); if (mf) { setModel(mf.dataset.model); return; }
  const sub = e.target.closest(".subtab"); if (sub) { S.sub = sub.dataset.sub; renderDetail(); return; }
  if (e.target.id === "close") { closeTrial(); return; }
  if (e.target.id === "clear-job") { S.job = ""; history.replaceState(null, "", hashFor()); render(); return; }
  const jv = e.target.closest("[data-jobview]"); if (jv && !e.target.closest("a")) { S.job = jv.dataset.jobview; S.tab = "trials"; history.replaceState(null, "", hashFor()); render(); window.scrollTo(0, 0); return; }
  const rr = e.target.closest("[data-round]"); if (rr) { openRound(rr.dataset.round); return; }
  if (e.target.id === "close-round") { closeRound(); return; }
  const jr = e.target.closest("#jobs tbody tr"); if (jr) { S.job = S.job === jr.dataset.job ? "" : jr.dataset.job; S.tab = "trials"; history.replaceState(null, "", hashFor()); render(); return; }
});
document.addEventListener("toggle", e => { const d = e.target; if (d.dataset && d.dataset.file && d.open) loadFile(d); }, true);
for (const id of ["f-job","f-kind","f-status"]) $("#"+id).addEventListener("change", e => { S[{ "f-job":"job","f-kind":"kind","f-status":"status" }[id]] = e.target.value; history.replaceState(null, "", hashFor()); render(); });
$("#f-model").addEventListener("change", e => setModel(e.target.value));
for (const id of ["t-search","t-diff","t-cat","t-split","t-seen"]) $("#"+id).addEventListener("input", () => { if (S.data) { renderTasks(S.data); postRender(); } });
for (const id of ["c-paired","c-sort"]) $("#"+id).addEventListener("input", () => { if (S.data) { renderCompare(S.data); postRender(); } });
$("#c-model").addEventListener("change", e => { S.cmpModel = e.target.value || null; if (S.data) { renderCompare(S.data); postRender(); } });
refresh().then(openFromHash);
setInterval(refresh, 4000);
</script>
"""


class Handler(BaseHTTPRequestHandler):
    root: Path = REPO / "bench" / "jobs"

    def _send(self, code, body, ctype):
        data = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):  # noqa: N802 (http.server's name)
        url = urlparse(self.path)
        q = parse_qs(url.query)
        arg = lambda k: q.get(k, [""])[0]  # noqa: E731
        try:
            if url.path == "/":
                self._send(200, PAGE, "text/html; charset=utf-8")
            elif url.path == "/api/summary":
                self._send(200, json.dumps(summary(self.root)), "application/json")
            elif url.path == "/api/trial":
                detail = trial_detail(self.root, arg("job"), arg("trial"))
                if detail is None:
                    self._send(404, '{"error": "no such trial"}', "application/json")
                else:
                    self._send(200, json.dumps(detail), "application/json")
            elif url.path == "/api/file":
                text = trial_file(self.root, arg("job"), arg("trial"), arg("path"))
                if text is None:
                    self._send(404, "no such file", "text/plain")
                else:
                    self._send(200, text, "text/plain; charset=utf-8")
            else:
                self._send(404, "not found", "text/plain")
        except BrokenPipeError:
            pass

    def log_message(self, *args):  # quiet
        pass


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root", type=Path, default=REPO / "bench" / "jobs")
    ap.add_argument("--port", type=int, default=8731)
    args = ap.parse_args()
    Handler.root = args.root.resolve()
    server = ThreadingHTTPServer(("localhost", args.port), Handler)
    print(f"gitagent dashboard: http://localhost:{args.port}  (watching {Handler.root})")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
