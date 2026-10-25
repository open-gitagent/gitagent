#!/usr/bin/env bash
# Run gitagent on the dev split, one Harbor job per agent time limit, all jobs at once.
#
#   scripts/run-dev-pass.sh <job-prefix> [profile.yaml] [skip-task ...]
#
# Jobs are named <job-prefix>-t<seconds> and written to bench/jobs. Only tasks
# listed under "dev" in bench/split.json are run; held-out tasks never are.
# The split file is kept on the benchmark machine, not in the repository.
# gitagent's --timeout-s is the task's agent timeout minus 60 s, so it stops
# cleanly before Harbor does. Concurrency per job: N_900 (default 6) for the
# large 900 s group, N_OTHER (default 3) for the rest. K (default 1) runs each
# task K times. Keep N_900 + 6 * N_OTHER under about 27: each trial gets its own
# Docker network and the default address pools run out beyond that.
# To re-run trials that failed to start, export the key first (harbor job
# resume does not reload --env-file):
#   set -a; . ./.env; set +a; harbor job resume -p bench/jobs/<job> -f <ErrorType>
#
# SPLIT=heldout runs the held-out list instead. That is the final scoring run
# and happens once, on frozen code, so it also needs CONFIRM_HELDOUT=yes.
# SPLIT=all runs all 89 tasks (dev and held-out), the leaderboard format with
# K=5; it includes the held-out tasks, so it needs CONFIRM_HELDOUT=yes too.
set -euo pipefail

prefix=${1:?usage: run-dev-pass.sh <job-prefix> [profile.yaml] [skip-task ...]}
profile=${2:-bench/profiles/anthropic-m4.yaml}
shift $(( $# >= 2 ? 2 : 1 ))
skip=" $* "
model=${MODEL:-anthropic/claude-opus-5-5}
tasks_dir=${TASKS_DIR:-$HOME/.cache/harbor/tasks/packages/terminal-bench}

cd "$(dirname "$0")/.."
export PATH="$HOME/.local/bin:$PATH"
[ -f .env ] || { echo "missing .env with ANTHROPIC_API_KEY" >&2; exit 1; }

split=${SPLIT:-dev}
case $split in
  dev) ;;
  heldout|all) [ "${CONFIRM_HELDOUT:-}" = yes ] || { echo "$split includes the held-out tasks: set CONFIRM_HELDOUT=yes" >&2; exit 1; } ;;
  *) echo "SPLIT must be dev, heldout or all" >&2; exit 1 ;;
esac
case $split in
  all) selector='.dev[], .heldout[]' ;;
  *) selector=".${split}[]" ;;
esac

declare -A groups
for t in $(jq -r "$selector" bench/split.json); do
  [[ $skip == *" $t "* ]] && continue
  toml=$(ls "$tasks_dir/$t"/*/task.toml | head -1)
  limit=$(awk '/^\[agent\]/{a=1;next} /^\[/{a=0} a && /^timeout_sec/{print int($3)}' "$toml")
  groups[$limit]+=" -i terminal-bench/$t"
done

pids=()
for limit in "${!groups[@]}"; do
  n=${N_OTHER:-3}; [ "$limit" = 900 ] && n=${N_900:-6}
  job="$prefix-t$limit"
  echo "$job: $(wc -w <<<"${groups[$limit]}" | awk '{print $1/2}') task(s), timeout_s=$((limit - 60)), -n $n"
  PYTHONPATH=$PWD harbor run -d terminal-bench/terminal-bench-2-1 -a gitagent_harbor.agent:GitAgent \
    -m "$model" --env-file .env --ak timeout_s=$((limit - 60)) --ak profile="$PWD/$profile" \
    -k "${K:-1}" -n "$n" -o bench/jobs --job-name "$job" -q ${groups[$limit]} > "bench/jobs/.$job.log" 2>&1 &
  pids+=($!)
done
status=0
for p in "${pids[@]}"; do wait "$p" || status=1; done
exit $status
