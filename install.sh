#!/usr/bin/env bash
set -euo pipefail

# ── Colors & Styles ──────────────────────────────────────────────
RESET=$'\e[0m'
BOLD=$'\e[1m'
DIM=$'\e[2m'
WHITE=$'\e[97m'
GREEN=$'\e[32m'
CYAN=$'\e[36m'
YELLOW=$'\e[33m'
NC=$'\e[0m'

# Truecolor support: VS Code, iTerm2, Ghostty, etc. set COLORTERM
if [[ "${COLORTERM:-}" =~ ^(truecolor|24bit)$ ]]; then
  EMPTY=$'\e[48;2;13;13;13m'
  OUTLINE=$'\e[48;2;18;7;11m'
  FILL=$'\e[48;2;255;79;99m'
  RED=$'\e[38;2;255;79;99m'
  GRAY=$'\e[38;2;160;160;160m'
  LGRAY=$'\e[38;2;110;110;110m'
else
  EMPTY=$'\e[40m'
  OUTLINE=$'\e[41m'
  FILL=$'\e[101m'
  RED=$'\e[91m'
  GRAY=$'\e[37m'
  LGRAY=$'\e[90m'
fi

# ── Sprite Banner ────────────────────────────────────────────────
rows=(
  "0 0 1 1 1 1 1 1 0 0"
  "0 1 2 2 2 2 2 2 1 0"
  "1 2 2 2 2 2 2 2 2 1"
  "1 2 1 2 2 2 2 1 2 1"
  "1 2 1 2 2 2 2 1 2 1"
  "1 2 2 2 2 2 2 2 2 1"
  "1 2 2 2 2 2 2 2 2 1"
  "0 1 2 2 2 2 2 2 1 0"
  "1 2 2 1 2 2 1 2 2 1"
  "0 1 1 0 1 1 0 1 1 0"
)

text=(
  ""
  ""
  "${RED}${BOLD}GitClaw v0.4.0${RESET}"
  "${GRAY}A universal git-native multimodal always learning AI Agent${RESET}"
  "${GRAY}(TinyHuman)${RESET}"
  ""
  "${LGRAY}Author   ${RESET}${WHITE}Shreyas Kapale @ Lyzr${RESET}"
  "${LGRAY}License  ${RESET}${WHITE}MIT${RESET}"
  ""
  "${DIM}${LGRAY}Powered by Lyzr Research Labs${RESET}"
)

clear
echo ""
for i in "${!rows[@]}"; do
  printf "  "
  for val in ${rows[$i]}; do
    case $val in
      0) printf "${EMPTY}  " ;;
      1) printf "${OUTLINE}  " ;;
      2) printf "${FILL}  " ;;
    esac
  done
  printf "${RESET}   "
  printf "${text[$i]}"
  printf "${RESET}\n"
done
echo ""
echo -e "  ${DIM}────────────────────────────────────────────────────${NC}"
echo ""

# ── Check prerequisites ──────────────────────────────────────────
echo -e "  ${BOLD}Checking prerequisites...${NC}"
echo ""

check_cmd() {
  if ! command -v "$1" &>/dev/null; then
    echo -e "  ${RED}✗ $1 is not installed${NC}"
    echo -e "    ${DIM}Install $1 and re-run this script.${NC}"
    exit 1
  fi
}

check_cmd node
check_cmd npm
check_cmd git

NODE_VERSION=$(node -v | sed 's/v//' | cut -d. -f1)
if [ "$NODE_VERSION" -lt 18 ]; then
  echo -e "  ${RED}✗ Node.js 18+ required (found $(node -v))${NC}"
  exit 1
fi

echo -e "  ${GREEN}✓${NC} node $(node -v)  ${GREEN}✓${NC} npm $(npm -v)  ${GREEN}✓${NC} git $(git --version | cut -d' ' -f3)"
echo ""

# ── Install gitclaw globally ─────────────────────────────────────
if command -v gitclaw &>/dev/null; then
  echo -e "  ${GREEN}✓${NC} gitclaw already installed"
else
  echo -e "  ${BOLD}Installing gitclaw...${NC}"
  # Remove corrupted partial installs that cause ENOTDIR
  NPM_GLOBAL_DIR="$(npm root -g 2>/dev/null || echo "")"
  if [ -n "$NPM_GLOBAL_DIR" ] && [ -d "${NPM_GLOBAL_DIR}/gitclaw" ] && [ ! -f "${NPM_GLOBAL_DIR}/gitclaw/package.json" ]; then
    rm -rf "${NPM_GLOBAL_DIR}/gitclaw"
  fi
  npm install -g gitclaw@latest 2>&1 | tail -2
  echo -e "  ${GREEN}✓${NC} gitclaw installed"
fi
echo ""

# ── Setup Mode Selection ─────────────────────────────────────────
echo -e "  ${BOLD}How would you like to set up?${NC}"
echo ""
echo -e "    ${RED}${BOLD}1)${NC} ${BOLD}Quick Setup${NC}     ${DIM}— OpenAI voice + Claude agent, get started in 30 seconds${NC}"
echo -e "    ${RED}${BOLD}2)${NC} ${BOLD}Advanced Setup${NC}  ${DIM}— choose voice adapter, model, project dir, integrations${NC}"
echo ""
read -rp "  Choice [1]: " SETUP_MODE
SETUP_MODE="${SETUP_MODE:-1}"
echo ""

# ═══════════════════════════════════════════════════════════════════
# QUICK SETUP
# ═══════════════════════════════════════════════════════════════════
if [ "$SETUP_MODE" = "1" ]; then

  echo -e "  ${DIM}────────────────────────────────────────────────────${NC}"
  echo -e "  ${RED}${BOLD}Quick Setup${NC}"
  echo -e "  ${DIM}Voice: OpenAI Realtime  •  Agent: Claude Sonnet 4.6${NC}"
  echo ""

  # OpenAI key
  echo -e "  ${BOLD}OpenAI API Key${NC} ${DIM}(for voice — get one at platform.openai.com)${NC}"
  read -rsp "  Key: " OPENAI_KEY
  echo ""
  if [ -z "$OPENAI_KEY" ]; then
    echo -e "  ${RED}✗ OpenAI key is required for voice mode${NC}"
    exit 1
  fi
  export OPENAI_API_KEY="$OPENAI_KEY"
  echo -e "  ${GREEN}✓${NC} OPENAI_API_KEY saved"

  # Anthropic key
  echo ""
  echo -e "  ${BOLD}Anthropic API Key${NC} ${DIM}(for agent brain — get one at console.anthropic.com)${NC}"
  read -rsp "  Key: " ANTHROPIC_KEY
  echo ""
  if [ -z "$ANTHROPIC_KEY" ]; then
    echo -e "  ${RED}✗ Anthropic key is required for the agent${NC}"
    exit 1
  fi
  export ANTHROPIC_API_KEY="$ANTHROPIC_KEY"
  echo -e "  ${GREEN}✓${NC} ANTHROPIC_API_KEY saved"

  # Composio key (optional)
  echo ""
  echo -e "  ${BOLD}Composio API Key${NC} ${DIM}(optional — enables Gmail, Calendar, Slack, GitHub)${NC}"
  read -rsp "  Key (press Enter to skip): " COMPOSIO_KEY
  echo ""
  if [ -n "$COMPOSIO_KEY" ]; then
    export COMPOSIO_API_KEY="$COMPOSIO_KEY"
    echo -e "  ${GREEN}✓${NC} COMPOSIO_API_KEY"
  else
    echo -e "  ${DIM}  skipped${NC}"
  fi

  ADAPTER="openai"
  ADAPTER_LABEL="OpenAI Realtime"
  MODEL="anthropic:claude-sonnet-4-6"
  PROJECT_DIR="${HOME}/assistant"

  # Create project dir and init git if needed
  mkdir -p "$PROJECT_DIR"
  if [ ! -d "$PROJECT_DIR/.git" ]; then
    git init -q "$PROJECT_DIR"
    echo -e "  ${GREEN}✓${NC} Initialized ~/assistant"
  fi

  echo ""

# ═══════════════════════════════════════════════════════════════════
# ADVANCED SETUP
# ═══════════════════════════════════════════════════════════════════
else

  echo -e "  ${DIM}────────────────────────────────────────────────────${NC}"
  echo -e "  ${RED}${BOLD}Advanced Setup${NC}"
  echo ""

  # ── Voice adapter ────────────────────────────────────────────
  echo -e "  ${BOLD}Voice Adapter${NC}"
  echo -e "    ${RED}1)${NC} OpenAI Realtime  ${DIM}(gpt-4o-realtime — best quality)${NC}"
  echo -e "    ${RED}2)${NC} Gemini Live      ${DIM}(gemini-2.0-flash — free tier available)${NC}"
  echo ""
  read -rp "  Choice [1]: " ADAPTER_CHOICE
  ADAPTER_CHOICE="${ADAPTER_CHOICE:-1}"

  if [ "$ADAPTER_CHOICE" = "2" ]; then
    ADAPTER="gemini"
    ADAPTER_LABEL="Gemini Live"
    KEY_ENV="GEMINI_API_KEY"
  else
    ADAPTER="openai"
    ADAPTER_LABEL="OpenAI Realtime"
    KEY_ENV="OPENAI_API_KEY"
  fi
  echo -e "  ${GREEN}✓${NC} ${ADAPTER_LABEL}"
  echo ""

  # ── API Keys ─────────────────────────────────────────────────
  echo -e "  ${BOLD}API Keys${NC}"
  echo -e "  ${DIM}Stored as environment variables for this session.${NC}"
  echo ""

  # Voice key
  echo -e "  ${BOLD}${KEY_ENV}${NC} ${DIM}(required for voice)${NC}"
  read -rsp "  Key: " VOICE_KEY
  echo ""
  if [ -z "$VOICE_KEY" ]; then
    echo -e "  ${RED}✗ ${KEY_ENV} is required for voice mode${NC}"
    exit 1
  fi
  export "$KEY_ENV=$VOICE_KEY"
  echo -e "  ${GREEN}✓${NC} ${KEY_ENV}"

  # Anthropic key
  echo ""
  echo -e "  ${BOLD}ANTHROPIC_API_KEY${NC} ${DIM}(required for agent)${NC}"
  read -rsp "  Key: " ANTHROPIC_KEY
  echo ""
  if [ -z "$ANTHROPIC_KEY" ]; then
    echo -e "  ${RED}✗ Anthropic key is required for the agent${NC}"
    exit 1
  fi
  export ANTHROPIC_API_KEY="$ANTHROPIC_KEY"
  echo -e "  ${GREEN}✓${NC} ANTHROPIC_API_KEY"

  # Composio key (optional)
  echo ""
  echo -e "  ${BOLD}COMPOSIO_API_KEY${NC} ${DIM}(optional — enables Gmail, Calendar, Slack, GitHub)${NC}"
  read -rsp "  Key (press Enter to skip): " COMPOSIO_KEY
  echo ""
  if [ -n "$COMPOSIO_KEY" ]; then
    export COMPOSIO_API_KEY="$COMPOSIO_KEY"
    echo -e "  ${GREEN}✓${NC} COMPOSIO_API_KEY"
  else
    echo -e "  ${DIM}  skipped${NC}"
  fi

  # Telegram token (optional)
  echo ""
  echo -e "  ${BOLD}TELEGRAM_BOT_TOKEN${NC} ${DIM}(optional — enables Telegram messaging)${NC}"
  read -rsp "  Token (press Enter to skip): " TELEGRAM_TOKEN
  echo ""
  if [ -n "$TELEGRAM_TOKEN" ]; then
    export TELEGRAM_BOT_TOKEN="$TELEGRAM_TOKEN"
    echo -e "  ${GREEN}✓${NC} TELEGRAM_BOT_TOKEN"
  else
    echo -e "  ${DIM}  skipped${NC}"
  fi
  echo ""

  # ── Project directory ────────────────────────────────────────
  echo -e "  ${BOLD}Project Directory${NC}"
  echo -e "  ${DIM}Where gitclaw will live — reads/writes files, runs commands.${NC}"
  read -rp "  Path [.]: " PROJECT_DIR
  PROJECT_DIR="${PROJECT_DIR:-.}"
  PROJECT_DIR="$(cd "$PROJECT_DIR" 2>/dev/null && pwd || echo "$PROJECT_DIR")"

  if [ ! -d "$PROJECT_DIR/.git" ]; then
    echo -e "  ${YELLOW}Not a git repo — initializing...${NC}"
    mkdir -p "$PROJECT_DIR"
    git -C "$PROJECT_DIR" init -q
  fi
  echo -e "  ${GREEN}✓${NC} ${PROJECT_DIR}"
  echo ""

  # ── Agent model ──────────────────────────────────────────────
  echo -e "  ${BOLD}Agent Model${NC} ${DIM}(the brain that executes tasks)${NC}"
  echo -e "    ${RED}1)${NC} claude-sonnet-4-6   ${DIM}(fast & capable — recommended)${NC}"
  echo -e "    ${RED}2)${NC} claude-opus-4-6     ${DIM}(most intelligent)${NC}"
  echo -e "    ${RED}3)${NC} custom"
  echo ""
  read -rp "  Choice [1]: " MODEL_CHOICE
  MODEL_CHOICE="${MODEL_CHOICE:-1}"

  case "$MODEL_CHOICE" in
    2) MODEL="anthropic:claude-opus-4-6" ;;
    3)
      read -rp "  Model name (provider:model): " MODEL
      ;;
    *) MODEL="anthropic:claude-sonnet-4-6" ;;
  esac
  echo -e "  ${GREEN}✓${NC} ${MODEL}"
  echo ""

  # ── Port ─────────────────────────────────────────────────────
  echo -e "  ${BOLD}Voice Server Port${NC}"
  read -rp "  Port [3333]: " PORT_INPUT
  PORT="${PORT_INPUT:-3333}"
  echo -e "  ${GREEN}✓${NC} Port ${PORT}"
  echo ""

fi

# ═══════════════════════════════════════════════════════════════════
# LAUNCH SUMMARY
# ═══════════════════════════════════════════════════════════════════
PORT="${PORT:-3333}"

echo -e "  ${DIM}────────────────────────────────────────────────────${NC}"
echo ""

# Summary box
echo -e "  ${RED}${BOLD}Ready to launch${NC}"
echo ""
echo -e "    ${LGRAY}Voice${NC}      ${WHITE}${ADAPTER_LABEL}${NC}"
echo -e "    ${LGRAY}Model${NC}      ${WHITE}${MODEL}${NC}"
echo -e "    ${LGRAY}Directory${NC}  ${WHITE}${PROJECT_DIR}${NC}"
echo -e "    ${LGRAY}Port${NC}       ${WHITE}${PORT}${NC}"
if [ -n "${COMPOSIO_API_KEY:-}" ]; then
  echo -e "    ${LGRAY}Composio${NC}   ${GREEN}enabled${NC}"
fi
if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
  echo -e "    ${LGRAY}Telegram${NC}   ${GREEN}enabled${NC}"
fi
echo ""
echo -e "  ${DIM}────────────────────────────────────────────────────${NC}"
echo ""
echo -e "  ${BOLD}Starting gitclaw...${NC}"
echo -e "  ${DIM}Opening ${CYAN}http://localhost:${PORT}${DIM} in your browser${NC}"
echo ""

# Save .env for future runs
ENV_FILE="${PROJECT_DIR}/.env"
{
  [ -n "${OPENAI_API_KEY:-}" ] && echo "OPENAI_API_KEY=${OPENAI_API_KEY}"
  [ -n "${GEMINI_API_KEY:-}" ] && echo "GEMINI_API_KEY=${GEMINI_API_KEY}"
  [ -n "${ANTHROPIC_API_KEY:-}" ] && echo "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}"
  [ -n "${COMPOSIO_API_KEY:-}" ] && echo "COMPOSIO_API_KEY=${COMPOSIO_API_KEY}"
  [ -n "${TELEGRAM_BOT_TOKEN:-}" ] && echo "TELEGRAM_BOT_TOKEN=${TELEGRAM_BOT_TOKEN}"
} > "$ENV_FILE"
echo -e "  ${GREEN}✓${NC} Keys saved to ${DIM}${ENV_FILE}${NC} ${DIM}(gitignored)${NC}"
echo ""

# Open browser after short delay
open_browser() {
  local url="http://localhost:${PORT}"
  if command -v open &>/dev/null; then
    open "$url"
  elif command -v xdg-open &>/dev/null; then
    xdg-open "$url"
  elif command -v start &>/dev/null; then
    start "$url"
  else
    echo -e "  ${YELLOW}Could not open browser automatically.${NC}"
    echo -e "  ${BOLD}Open this URL manually:${NC} ${CYAN}${url}${NC}"
  fi
}

echo -e "  ${RED}${BOLD}▶${NC} ${BOLD}http://localhost:${PORT}${NC}"
echo ""

(sleep 2 && open_browser) &

exec gitclaw --model "$MODEL" --voice --dir "$PROJECT_DIR"
