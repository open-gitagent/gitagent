# HOL Guard integration

This is a native `pre_tool_use` plugin for GitAgent's `cli` tool. It sends each shell command to HOL Guard before GitAgent executes it and blocks the tool call when Guard denies it, requires review, fails, times out, or returns an unrecognized decision.

The integration is intentionally narrow: it protects GitAgent shell execution and does not claim that unrelated tools are automatically covered.

## Requirements

Install HOL Guard in an isolated CLI environment:

```bash
pipx install hol-guard
```

The plugin invokes the installed `hol-guard` executable directly. No replacement policy engine is implemented in GitAgent.

## Install from a GitAgent checkout

Copy this directory into the agent's local plugin directory:

```bash
mkdir -p /path/to/agent/plugins/hol-guard
cp -R integrations/hol-guard/. /path/to/agent/plugins/hol-guard/
```

Then enable it in `agent.yaml`:

```yaml
plugins:
  hol-guard:
    enabled: true
    config:
      binary: hol-guard
      workspace: /path/to/agent
```

`HOL_GUARD_BIN` and `HOL_GUARD_HOME` can also provide the executable and Guard state directory.

## Decision mapping

The plugin calls HOL Guard's hook runtime using a `PreToolUse` payload for the GitAgent `cli` command. Guard remains the policy authority.

- Guard `allow` -> GitAgent allows the command.
- Guard `deny`/`block` -> GitAgent blocks the command.
- Guard `ask`/`review` -> GitAgent blocks the command until the review is resolved outside the tool call.
- Guard timeout, launch failure, malformed output, or unknown decision -> GitAgent blocks the command (fail closed).

GitAgent's hook contract supports `allow`, `block`, and `modify`, but it has no native pending-review state, so Guard review decisions are conservatively mapped to `block`.
