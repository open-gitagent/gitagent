#!/usr/bin/env bash
set -euo pipefail

# ── Colors ──────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

clear
echo ""
echo -e "${BOLD}  ╔══════════════════════════════════════╗${NC}"
echo -e "${BOLD}  ║         ${CYAN}GitClaw${NC}${BOLD} Installer            ║${NC}"
echo -e "${BOLD}  ║   Voice + Multimodal AI Agent        ║${NC}"
echo -e "${BOLD}  ╚══════════════════════════════════════╝${NC}"
echo ""

# ── Check prerequisites ────────────────────────────────────────────
check_cmd() {
  if ! command -v "$1" &>/dev/null; then
    echo -e "${RED}Error: $1 is required but not installed.${NC}"
    echo -e "${DIM}Install it and re-run this script.${NC}"
    exit 1
  fi
}

check_cmd node
check_cmd npm
check_cmd git

NODE_VERSION=$(node -v | sed 's/v//' | cut -d. -f1)
if [ "$NODE_VERSION" -lt 18 ]; then
  echo -e "${RED}Error: Node.js 18+ required (found $(node -v))${NC}"
  exit 1
fi

echo -e "${GREEN}✓${NC} Node $(node -v), npm $(npm -v), git $(git --version | cut -d' ' -f3)"
echo ""

# ── Install gitclaw ────────────────────────────────────────────────
echo -e "${BOLD}Installing gitclaw...${NC}"
npm install -g gitclaw 2>&1 | tail -3
echo -e "${GREEN}✓${NC} gitclaw installed ($(gitclaw --version 2>/dev/null || echo 'latest'))"
echo ""

# ── Voice adapter selection ────────────────────────────────────────
echo -e "${BOLD}Select voice adapter:${NC}"
echo -e "  ${CYAN}1)${NC} OpenAI Realtime  ${DIM}(gpt-4o-realtime-preview)${NC}"
echo -e "  ${CYAN}2)${NC} Gemini Live      ${DIM}(gemini-2.0-flash-exp)${NC}"
echo ""
read -rp "Choice [1]: " ADAPTER_CHOICE
ADAPTER_CHOICE="${ADAPTER_CHOICE:-1}"

if [ "$ADAPTER_CHOICE" = "2" ]; then
  ADAPTER="gemini"
  ADAPTER_LABEL="Gemini Live"
  KEY_ENV="GEMINI_API_KEY"
  DEFAULT_MODEL="gemini-2.0-flash-exp"
else
  ADAPTER="openai"
  ADAPTER_LABEL="OpenAI Realtime"
  KEY_ENV="OPENAI_API_KEY"
  DEFAULT_MODEL="gpt-4o-realtime-preview"
fi

echo -e "${GREEN}✓${NC} ${ADAPTER_LABEL}"
echo ""

# ── API Keys ───────────────────────────────────────────────────────
echo -e "${BOLD}API Keys${NC}"
echo -e "${DIM}Keys are stored as environment variables for this session only.${NC}"
echo ""

# Voice adapter key (required)
EXISTING_KEY="${!KEY_ENV:-}"
if [ -n "$EXISTING_KEY" ]; then
  echo -e "${KEY_ENV}: ${GREEN}already set${NC}"
  VOICE_KEY="$EXISTING_KEY"
else
  read -rsp "${KEY_ENV} (required): " VOICE_KEY
  echo ""
  if [ -z "$VOICE_KEY" ]; then
    echo -e "${RED}Error: ${KEY_ENV} is required for voice mode.${NC}"
    exit 1
  fi
fi
export "$KEY_ENV=$VOICE_KEY"
echo -e "${GREEN}✓${NC} ${KEY_ENV}"

# Anthropic key (for agent tool execution)
EXISTING_ANTHROPIC="${ANTHROPIC_API_KEY:-}"
if [ -n "$EXISTING_ANTHROPIC" ]; then
  echo -e "ANTHROPIC_API_KEY: ${GREEN}already set${NC}"
else
  echo ""
  read -rsp "ANTHROPIC_API_KEY (for agent, optional): " ANTHROPIC_KEY
  echo ""
  if [ -n "$ANTHROPIC_KEY" ]; then
    export ANTHROPIC_API_KEY="$ANTHROPIC_KEY"
    echo -e "${GREEN}✓${NC} ANTHROPIC_API_KEY"
  else
    echo -e "${DIM}  skipped${NC}"
  fi
fi

# Composio key (for integrations)
EXISTING_COMPOSIO="${COMPOSIO_API_KEY:-}"
if [ -n "$EXISTING_COMPOSIO" ]; then
  echo -e "COMPOSIO_API_KEY: ${GREEN}already set${NC}"
else
  echo ""
  read -rsp "COMPOSIO_API_KEY (for integrations, optional): " COMPOSIO_KEY
  echo ""
  if [ -n "$COMPOSIO_KEY" ]; then
    export COMPOSIO_API_KEY="$COMPOSIO_KEY"
    echo -e "${GREEN}✓${NC} COMPOSIO_API_KEY"
  else
    echo -e "${DIM}  skipped${NC}"
  fi
fi

echo ""

# ── Project directory ──────────────────────────────────────────────
echo -e "${BOLD}Project directory${NC}"
echo -e "${DIM}The folder gitclaw operates in (reads/writes files, runs commands).${NC}"
read -rp "Path [.]: " PROJECT_DIR
PROJECT_DIR="${PROJECT_DIR:-.}"
PROJECT_DIR="$(cd "$PROJECT_DIR" 2>/dev/null && pwd || echo "$PROJECT_DIR")"

# Init git repo if not one
if [ ! -d "$PROJECT_DIR/.git" ]; then
  echo -e "${YELLOW}Not a git repo. Initializing...${NC}"
  mkdir -p "$PROJECT_DIR"
  git -C "$PROJECT_DIR" init -q
fi

echo -e "${GREEN}✓${NC} ${PROJECT_DIR}"
echo ""

# ── Agent model ────────────────────────────────────────────────────
echo -e "${BOLD}Agent model${NC} ${DIM}(for tool execution / code tasks)${NC}"
echo -e "  ${CYAN}1)${NC} claude-sonnet-4-20250514  ${DIM}(default)${NC}"
echo -e "  ${CYAN}2)${NC} claude-opus-4-20250514"
echo -e "  ${CYAN}3)${NC} custom"
echo ""
read -rp "Choice [1]: " MODEL_CHOICE
MODEL_CHOICE="${MODEL_CHOICE:-1}"

case "$MODEL_CHOICE" in
  2) MODEL="claude-opus-4-20250514" ;;
  3)
    read -rp "Model name: " MODEL
    ;;
  *) MODEL="claude-sonnet-4-20250514" ;;
esac

echo -e "${GREEN}✓${NC} ${MODEL}"
echo ""

# ── Summary ────────────────────────────────────────────────────────
echo -e "${BOLD}┌─────────────────────────────────────┐${NC}"
echo -e "${BOLD}│${NC}  Voice:    ${CYAN}${ADAPTER_LABEL}${NC}"
echo -e "${BOLD}│${NC}  Model:    ${CYAN}${MODEL}${NC}"
echo -e "${BOLD}│${NC}  Dir:      ${CYAN}${PROJECT_DIR}${NC}"
echo -e "${BOLD}│${NC}  Composio: ${CYAN}${COMPOSIO_API_KEY:+enabled}${COMPOSIO_API_KEY:-disabled}${NC}"
echo -e "${BOLD}└─────────────────────────────────────┘${NC}"
echo ""

# ── Launch ─────────────────────────────────────────────────────────
PORT="${PORT:-3333}"
echo -e "${BOLD}Starting voice server on port ${PORT}...${NC}"
echo ""

# Open browser after a short delay
(sleep 2 && open "http://localhost:${PORT}" 2>/dev/null || xdg-open "http://localhost:${PORT}" 2>/dev/null || true) &

exec gitclaw --dir "$PROJECT_DIR" --model "$MODEL" --voice "$ADAPTER"
