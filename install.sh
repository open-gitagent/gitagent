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
  "${RED}${BOLD}GitClaw v1.1.1${RESET}"
  "${GRAY}A universal git-native multimodal always learning AI Agent${RESET}"
  "${GRAY}(TinyHuman)${RESET}"
  ""
  "${LGRAY}Author   ${RESET}${WHITE}Shreyas Kapale @ Lyzr Research Labs${RESET}"
  "${LGRAY}License  ${RESET}${WHITE}MIT${RESET}"
  ""
  "${DIM}${LGRAY}A product of Lyzr Research Labs${RESET}"
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

# ── Install / update gitclaw globally ────────────────────────────
if command -v gitclaw &>/dev/null; then
  INSTALLED_VER="$(npm ls -g gitclaw --depth=0 --json 2>/dev/null | node -pe "JSON.parse(require('fs').readFileSync('/dev/stdin','utf8')).dependencies?.gitclaw?.version || ''" 2>/dev/null || echo "")"
  LATEST_VER="$(npm view gitclaw version 2>/dev/null || echo "")"

  if [ -n "$INSTALLED_VER" ] && [ -n "$LATEST_VER" ] && [ "$INSTALLED_VER" != "$LATEST_VER" ]; then
    echo -e "  ${YELLOW}⬆${NC}  gitclaw ${DIM}v${INSTALLED_VER}${NC} installed — ${GREEN}v${LATEST_VER}${NC} available"
    read -rp "  Update to v${LATEST_VER}? [Y/n]: " UPDATE_CHOICE
    UPDATE_CHOICE="${UPDATE_CHOICE:-Y}"
    if [[ "$UPDATE_CHOICE" =~ ^[Yy] ]]; then
      echo -e "  ${BOLD}Updating gitclaw...${NC}"
      npm install -g gitclaw@latest 2>&1 | tail -2
      echo -e "  ${GREEN}✓${NC} gitclaw updated to v${LATEST_VER}"
    else
      echo -e "  ${DIM}  keeping v${INSTALLED_VER}${NC}"
    fi
  else
    echo -e "  ${GREEN}✓${NC} gitclaw v${INSTALLED_VER:-latest} ${DIM}(up to date)${NC}"
  fi
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

# ── Auto-resume existing setup ──────────────────────────────────
PROJECT_DIR="${HOME}/assistant"
if [ -d "$PROJECT_DIR" ] && [ -f "$PROJECT_DIR/agent.yaml" ]; then
  echo -e "  ${GREEN}✓${NC} Found existing assistant at ${DIM}${PROJECT_DIR}${NC}"

  # Re-export .env keys into current shell (strip Windows \r line endings)
  if [ -f "$PROJECT_DIR/.env" ]; then
    sed -i.bak 's/\r$//' "$PROJECT_DIR/.env" && rm -f "$PROJECT_DIR/.env.bak"
    set -a
    source "$PROJECT_DIR/.env"
    set +a
    echo -e "  ${GREEN}✓${NC} Loaded keys from ${DIM}${PROJECT_DIR}/.env${NC}"
  fi

  # Prompt for missing required keys (skip if Lyzr is configured)
  if [ -n "${GITCLAW_LYZR_AGENT_ID:-}" ]; then
    echo -e "  ${GREEN}✓${NC} Lyzr agent: ${DIM}${GITCLAW_LYZR_AGENT_ID}${NC}"
  else
    if [ -z "${OPENAI_API_KEY:-}" ] && [ -z "${GEMINI_API_KEY:-}" ]; then
      echo ""
      echo -e "  ${YELLOW}⚠${NC}  No voice API key found."
      echo -e "  ${BOLD}OpenAI API Key${NC} ${DIM}(for voice — get one at platform.openai.com)${NC}"
      read -rsp "  Key: " OPENAI_KEY
      echo ""
      if [ -n "$OPENAI_KEY" ]; then
        export OPENAI_API_KEY="$OPENAI_KEY"
        echo -e "  ${GREEN}✓${NC} OPENAI_API_KEY saved"
        echo "OPENAI_API_KEY=${OPENAI_API_KEY}" >> "$PROJECT_DIR/.env"
      else
        echo -e "  ${DIM}  skipped — text-only mode${NC}"
      fi
    fi

    if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
      echo ""
      echo -e "  ${YELLOW}⚠${NC}  No Anthropic API key found."
      echo -e "  ${BOLD}Anthropic API Key${NC} ${DIM}(for agent brain — get one at console.anthropic.com)${NC}"
      read -rsp "  Key: " ANTHROPIC_KEY
      echo ""
      if [ -z "$ANTHROPIC_KEY" ]; then
        echo -e "  ${RED}✗ Anthropic key is required for the agent${NC}"
        exit 1
      fi
      export ANTHROPIC_API_KEY="$ANTHROPIC_KEY"
      echo -e "  ${GREEN}✓${NC} ANTHROPIC_API_KEY saved"
      echo "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}" >> "$PROJECT_DIR/.env"
    fi
  fi

  # Set model — use Lyzr if configured, otherwise let loadAgent() read from agent.yaml
  if [ -n "${GITCLAW_LYZR_AGENT_ID:-}" ]; then
    MODEL="lyzr:${GITCLAW_LYZR_AGENT_ID}@https://agent-prod.studio.lyzr.ai/v4/chat"
  else
    MODEL=""
  fi

  # Determine adapter from available keys
  if [ -n "${GITCLAW_LYZR_AGENT_ID:-}" ] && [ -z "${OPENAI_API_KEY:-}" ]; then
    ADAPTER_LABEL="Text Only (Lyzr)"
  elif [ -n "${GEMINI_API_KEY:-}" ] && [ -z "${OPENAI_API_KEY:-}" ]; then
    ADAPTER_LABEL="Gemini Live"
  elif [ -n "${OPENAI_API_KEY:-}" ]; then
    ADAPTER_LABEL="OpenAI Realtime"
  else
    ADAPTER_LABEL="Text Only"
  fi

  PORT="${PORT:-3333}"

  echo -e "  ${DIM}Resuming with: ${MODEL} on port ${PORT}${NC}"
  echo ""

else

# ── Setup Mode Selection ─────────────────────────────────────────
echo -e "  ${BOLD}How would you like to run?${NC}"
echo ""
echo -e "    ${RED}${BOLD}1)${NC} ${BOLD}Install with LYZR${NC} ${DIM}— powered by Lyzr AI Studio (easiest)${NC}"
echo -e "    ${RED}${BOLD}2)${NC} ${BOLD}Voice + Text${NC}    ${DIM}— real-time voice chat + text (requires OpenAI key)${NC}"
echo -e "    ${RED}${BOLD}3)${NC} ${BOLD}Text Only${NC}       ${DIM}— text chat only, no voice (just Anthropic key)${NC}"
echo -e "    ${RED}${BOLD}4)${NC} ${BOLD}Advanced Setup${NC}  ${DIM}— choose voice adapter, model, project dir, integrations${NC}"
echo ""
read -rp "  Choice [1]: " SETUP_MODE
SETUP_MODE="${SETUP_MODE:-1}"
echo ""

# ═══════════════════════════════════════════════════════════════════
# QUICK SETUP
# ═══════════════════════════════════════════════════════════════════
# ═══════════════════════════════════════════════════════════════════
# LYZR SETUP
# ═══════════════════════════════════════════════════════════════════
if [ "$SETUP_MODE" = "1" ]; then

  echo -e "  ${DIM}────────────────────────────────────────────────────${NC}"
  echo -e "  ${RED}${BOLD}Install with LYZR${NC}"
  echo -e "  ${DIM}Powered by Lyzr AI Studio — agent brain runs on Lyzr cloud${NC}"
  echo ""

  # LYZR API key
  echo -e "  ${BOLD}Lyzr API Key${NC} ${DIM}(get one at studio.lyzr.ai)${NC}"
  read -rsp "  Key: " LYZR_KEY
  echo ""
  if [ -z "$LYZR_KEY" ]; then
    echo -e "  ${RED}✗ Lyzr API key is required${NC}"
    exit 1
  fi
  export LYZR_API_KEY="$LYZR_KEY"
  echo -e "  ${GREEN}✓${NC} LYZR_API_KEY saved"

  # Check if agent already exists
  if [ -z "${GITCLAW_LYZR_AGENT_ID:-}" ]; then
    echo ""
    echo -e "  ${DIM}Creating Lyzr agent...${NC}"
    LYZR_RESPONSE=$(curl -s -X POST 'https://agent-prod.studio.lyzr.ai/v3/agents/' \
      -H 'accept: application/json' \
      -H 'content-type: application/json' \
      -H "x-api-key: ${LYZR_API_KEY}" \
      --data-raw '{
        "name": "GitClaw Assistant",
        "description": "GitClaw AI agent powered by Lyzr",
        "agent_role": "",
        "agent_goal": "",
        "agent_instructions": "",
        "examples": null,
        "tools": [],
        "tool_usage_description": "{}",
        "tool_configs": [],
        "provider_id": "Anthropic",
        "model": "anthropic/claude-sonnet-4-6",
        "temperature": 0.7,
        "top_p": 0.9,
        "llm_credential_id": "lyzr_anthropic",
        "features": [],
        "managed_agents": [],
        "a2a_tools": [],
        "additional_model_params": null,
        "response_format": {"type": "text"},
        "store_messages": true,
        "file_output": false,
        "image_output_config": null,
        "max_iterations": 25
      }' 2>/dev/null)

    # Extract agent ID from response
    LYZR_AGENT_ID=$(echo "$LYZR_RESPONSE" | grep -o '"agent_id"\s*:\s*"[^"]*"' | head -1 | sed 's/.*"agent_id"\s*:\s*"\([^"]*\)".*/\1/')
    if [ -z "$LYZR_AGENT_ID" ]; then
      # Try alternate field name
      LYZR_AGENT_ID=$(echo "$LYZR_RESPONSE" | grep -o '"id"\s*:\s*"[^"]*"' | head -1 | sed 's/.*"id"\s*:\s*"\([^"]*\)".*/\1/')
    fi

    if [ -z "$LYZR_AGENT_ID" ]; then
      echo -e "  ${RED}✗ Failed to create Lyzr agent${NC}"
      echo -e "  ${DIM}Response: ${LYZR_RESPONSE}${NC}"
      exit 1
    fi

    export GITCLAW_LYZR_AGENT_ID="$LYZR_AGENT_ID"
    echo -e "  ${GREEN}✓${NC} Agent created: ${DIM}${LYZR_AGENT_ID}${NC}"
  else
    echo -e "  ${GREEN}✓${NC} Using existing agent: ${DIM}${GITCLAW_LYZR_AGENT_ID}${NC}"
  fi

  # OpenAI key for voice (optional)
  echo ""
  echo -e "  ${BOLD}OpenAI API Key${NC} ${DIM}(optional — for voice mode, press Enter to skip)${NC}"
  read -rsp "  Key: " OPENAI_KEY
  echo ""
  if [ -n "$OPENAI_KEY" ]; then
    export OPENAI_API_KEY="$OPENAI_KEY"
    echo -e "  ${GREEN}✓${NC} OPENAI_API_KEY saved"
    VOICE_ENABLED=true
  else
    echo -e "  ${DIM}  skipped — text-only mode${NC}"
    VOICE_ENABLED=false
  fi

  # Set model to use Lyzr completions endpoint with agent ID as model
  MODEL="lyzr:${GITCLAW_LYZR_AGENT_ID}@https://agent-prod.studio.lyzr.ai/v4/chat"
  export GITCLAW_MODEL_BASE_URL="https://agent-prod.studio.lyzr.ai/v4/chat"
  ADAPTER_LABEL="${VOICE_ENABLED:+OpenAI Realtime}${VOICE_ENABLED:-Text Only}"
  if [ "$VOICE_ENABLED" = true ]; then
    ADAPTER_LABEL="OpenAI Realtime"
  else
    ADAPTER_LABEL="Text Only (Lyzr)"
  fi
  PROJECT_DIR="${HOME}/assistant"

  # Create project dir and init git if needed
  mkdir -p "$PROJECT_DIR"
  if [ ! -d "$PROJECT_DIR/.git" ]; then
    git init -q "$PROJECT_DIR"
    echo -e "  ${GREEN}✓${NC} Initialized ~/assistant"
  fi

  echo ""

# ═══════════════════════════════════════════════════════════════════
# VOICE + TEXT / TEXT ONLY SETUP
# ═══════════════════════════════════════════════════════════════════
elif [ "$SETUP_MODE" = "2" ] || [ "$SETUP_MODE" = "3" ]; then

  VOICE_ENABLED=true
  if [ "$SETUP_MODE" = "3" ]; then
    VOICE_ENABLED=false
  fi

  echo -e "  ${DIM}────────────────────────────────────────────────────${NC}"
  if [ "$VOICE_ENABLED" = true ]; then
    echo -e "  ${RED}${BOLD}Voice + Text Setup${NC}"
    echo -e "  ${DIM}Voice: OpenAI Realtime  •  Agent: Claude Sonnet 4.6${NC}"
  else
    echo -e "  ${RED}${BOLD}Text Only Setup${NC}"
    echo -e "  ${DIM}Agent: Claude Sonnet 4.6  •  No voice, text chat via browser${NC}"
  fi
  echo ""

  # OpenAI key (required for voice, optional for text-only)
  if [ "$VOICE_ENABLED" = true ]; then
    echo -e "  ${BOLD}OpenAI API Key${NC} ${DIM}(for voice — get one at platform.openai.com)${NC}"
    read -rsp "  Key: " OPENAI_KEY
    echo ""
    if [ -z "$OPENAI_KEY" ]; then
      echo -e "  ${RED}✗ OpenAI key is required for voice mode${NC}"
      exit 1
    fi
    export OPENAI_API_KEY="$OPENAI_KEY"
    echo -e "  ${GREEN}✓${NC} OPENAI_API_KEY saved"
  fi

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
  if [ "$VOICE_ENABLED" = true ]; then
    ADAPTER_LABEL="OpenAI Realtime"
  else
    ADAPTER_LABEL="Text Only"
  fi
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

fi  # end auto-resume / interactive setup

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
if [ -n "${GITCLAW_LYZR_AGENT_ID:-}" ]; then
  echo -e "    ${LGRAY}Lyzr${NC}       ${GREEN}enabled${NC} ${DIM}(agent: ${GITCLAW_LYZR_AGENT_ID})${NC}"
fi
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
  [ -n "${LYZR_API_KEY:-}" ] && echo "LYZR_API_KEY=${LYZR_API_KEY}"
  [ -n "${GITCLAW_LYZR_AGENT_ID:-}" ] && echo "GITCLAW_LYZR_AGENT_ID=${GITCLAW_LYZR_AGENT_ID}"
  [ -n "${GITCLAW_MODEL_BASE_URL:-}" ] && echo "GITCLAW_MODEL_BASE_URL=${GITCLAW_MODEL_BASE_URL}"
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

if [ -n "$MODEL" ]; then
  exec gitclaw --model "$MODEL" --voice --dir "$PROJECT_DIR"
else
  exec gitclaw --voice --dir "$PROJECT_DIR"
fi
