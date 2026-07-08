# Gitagent Desktop (macOS)

A Cowork-style desktop app for [gitagent](../README.md): pick a local folder, give a
goal, and the agent works autonomously in a **session** — with model choice, streaming
progress, tool activity, and **plan approval** before any changes.

It's an Electron app that embeds the gitagent SDK **in-process** in the main process
(`query()` runs inline), with the React renderer talking to it over IPC.

## Architecture

```
main process (Node)                 renderer (React)
  agent-runner.ts ── query() ──┐
  ipc.ts / settings.ts         │  IPC   src/renderer/App.tsx
  @open-gitagent/gitagent  ────┼──────► transcript · tool activity
                               │        plan approval · permission modal
  preload/index.ts (contextBridge) ◄────┘
```

- **Sessions** = a local folder + a `gitagent/session-<hash>` git branch
  (`initLocalFolderSession`). No GitHub/PAT required.
- **Isolation** = every file tool is folder-jailed to the session directory
  (`query({ rootDir })`) and gated by the permission + plan-mode layer.
- **Models/keys** = the Settings pane writes API keys to `~/.gitagent/.env` (the SDK's
  global env fallback) and threads the chosen `provider:model` into `query()`.

## Develop

```bash
# From the repo root — build the SDK first (the app imports the built dist):
npm install            # installs the desktop workspace too (Electron, React…)
npm run build          # build @open-gitagent/gitagent

# Run the app in dev (hot-reload renderer):
npm --workspace @open-gitagent/desktop run dev
```

## Package a .dmg

```bash
npm --workspace @open-gitagent/desktop run dist
# → desktop/release/Gitagent-<version>.dmg
```

## Status

- **D1/D2 (done):** SDK folder-jail + local-folder sessions; Electron shell; single
  session with folder pick, model settings, streaming transcript, tool activity, plan
  approval, and permission prompts.
- **Next (D3+):** multi-session sidebar (list/switch/resume via chat-history),
  persisted transcripts, per-session permission rules, file/diff viewer, scheduling UI,
  and (later) real local VM/container isolation for Cowork parity.
