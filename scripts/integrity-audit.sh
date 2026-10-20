#!/usr/bin/env bash
# Checks that nothing in the agent's behavior depends on benchmark tasks:
#  1. no Terminal-Bench task name appears in prompts, profiles, config or source;
#  2. no reference to verifier code, reference solutions or reading /tests;
#  3. the system prompt and tool list contain nothing task-specific.
# Task names come from Harbor's local task cache (or a file given as $1).
# Documentation and benchmark records are not checked: they name the tasks
# used for checks on purpose.
set -euo pipefail
cd "$(dirname "$0")/.."

checked=(prompts profiles gitagent.yaml crates gitagent_harbor bench/profiles bench/fixtures)
status=0

names_file=$(mktemp)
trap 'rm -f "$names_file"' EXIT
if [ $# -ge 1 ]; then
  cp "$1" "$names_file"
else
  cache="${HOME}/.cache/harbor/tasks/packages/terminal-bench"
  if [ -d "$cache" ]; then
    ls "$cache" > "$names_file"
  fi
fi
count=$(wc -l < "$names_file" | tr -d ' ')
echo "1. task names ($count known)"
if [ "$count" -eq 0 ]; then
  echo "   skipped: no task list (pass a file of names, or download the dataset with harbor)"
else
  while read -r name; do
    [ -z "$name" ] && continue
    if grep -rIl --exclude-dir=target -e "$name" "${checked[@]}" 2>/dev/null; then
      echo "   FOUND task name: $name"
      status=1
    fi
  done < "$names_file"
  [ $status -eq 0 ] && echo "   ok: none found"
fi

echo "2. verifier, solution and /tests references"
hits=$(grep -rInE --exclude-dir=target \
  -e 'verifier|solution\.sh|solve\.sh|reward\.txt|/tests/test_' \
  "${checked[@]}" 2>/dev/null \
  | grep -vE 'never (read|write)|Never read|never_read|\["verifier_result"\]|nothing may be written|PROTECTED_DIR|writes_under_tests|protected_writes|Do not modify or create anything under `/tests`|gitagent_harbor/agent.py:.*(Never touches|never touches)|test_x\.py' || true)
if [ -n "$hits" ]; then
  echo "$hits" | sed 's/^/   /'
  echo "   review the lines above"
  status=1
else
  echo "   ok: only the /tests write guard and the rule in the prompt"
fi

echo "3. task-specific text in the system prompt (as sent: header comments removed)"
if for f in prompts/*.md; do awk 'NR==1 && /^<!--/ {skip=1} skip && /-->/ {skip=0; next} !skip' "$f"; done \
  | grep -nE 'Terminal-Bench|terminal-bench|[Hh]arbor|benchmark'; then
  echo "   FOUND benchmark-specific wording in prompts"
  status=1
else
  echo "   ok"
fi

exit $status
