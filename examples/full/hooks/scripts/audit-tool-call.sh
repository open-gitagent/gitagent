#!/usr/bin/env bash
# Audit hook: Log tool invocation to immutable audit trail
# Input: JSON on stdin with event details
# Output: JSON on stdout with action (allow/block/modify)

set -euo pipefail

INPUT=$(cat)
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
AUDIT_DIR="${GITAGENT_AUDIT_DIR:-.gitagent/audit}"

mkdir -p "$AUDIT_DIR"

# Extract event details
TOOL_NAME=$(echo "$INPUT" | jq -r '.data.tool_name // "unknown"')
SESSION_ID=$(echo "$INPUT" | jq -r '.session.id // "unknown"')
MODEL_VERSION=$(echo "$INPUT" | jq -r '.session.model_version // "unknown"')

# Write audit entry (append-only)
AUDIT_ENTRY=$(jq -n \
  --arg ts "$TIMESTAMP" \
  --arg event "pre_tool_use" \
  --arg tool "$TOOL_NAME" \
  --arg session "$SESSION_ID" \
  --arg model "$MODEL_VERSION" \
  --argjson input "$INPUT" \
  '{
    timestamp: $ts,
    event: $event,
    tool_name: $tool,
    session_id: $session,
    model_version: $model,
    details: $input
  }')

echo "$AUDIT_ENTRY" >> "$AUDIT_DIR/audit-$(date -u +%Y%m%d).jsonl"

# Return allow action
jq -n '{
  action: "allow",
  modifications: null,
  audit: {
    logged: true,
    timestamp: "'"$TIMESTAMP"'"
  }
}'
