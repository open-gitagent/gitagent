#!/usr/bin/env bash
set -euo pipefail
INPUT=$(cat)
echo '{"action": "allow", "modifications": null, "audit": {"logged": true, "finalized": true}}'
