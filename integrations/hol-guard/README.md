# HOL Guard pre-tool example for GitAgent

This opt-in example is a GitAgent plugin that checks commands passed to the native `cli` tool before they execute. It uses GitAgent's existing `pre_tool_use` hook. It is a source contribution under [PR #107](https://github.com/open-gitagent/gitagent/pull/107), not an approved or shipped GitAgent plugin.

## Setup

Install [HOL Guard](https://github.com/hashgraph-online/hol-guard) as a separate local CLI and verify it is available on `PATH`:

```bash
hol-guard --version
hol-guard command test 'git status' --json
```

From a GitAgent source checkout containing this integration, install the plugin by its local directory:

```bash
gitagent plugin install ./integrations/hol-guard
gitagent plugin list
```

The directory contains `plugin.yaml` and `index.mjs`. The plugin enables a programmatic hook; it does not require changing GitAgent's default tool implementation. You can disable it with `gitagent plugin disable hol-guard`, or remove it through the regular GitAgent plugin command.

The plugin requires an explicit **absolute** CLI execution directory. Before running GitAgent with the plugin enabled, supply the workspace used by its built-in `cli` tool:

```bash
export HOL_GUARD_WORKSPACE="/absolute/path/to/your/agent-directory"
```

The manifest declares `workspace` as required and maps `HOL_GUARD_WORKSPACE` to that config field. GitAgent's manifest loader warns when required configuration is missing; the Guard callback itself **blocks** protected `cli` calls if the directory is unset, relative, or cannot be verified. The directory must match where `cli` actually executes, **not** the plugin installation directory or the launcher's unrelated checkout. Explicit per-plugin user configuration can also supply `workspace`. Other fields include `binary` (or `HOL_GUARD_BIN`), `guard_home` and `timeout_ms` (default 6,000). If the Guard CLI cannot be started, returns invalid or contradictory JSON, requests review, or times out, the `cli` call is also blocked.

## Security and coverage limits

**This is a conservative command classifier, not full HOL Guard runtime policy enforcement.** The plugin calls `hol-guard command test --json -- <command>`, which inspects a command without executing it or evaluating saved policy and approvals (`policy_evaluation: "not_run"`). Only `minimum_action: "allow"` together with `classification.explicitly_benign: true` is accepted; all other results block. There is no Guard approval queue, no native runtime receipt, and no evidence of a managed Guard harness installation.

**Credential exposure limitation:** This example passes the complete shell command to `hol-guard command test` as a subprocess **argument**, not through a private stdin channel. On operating systems where other local users, debugging tools, or process collectors can read process command lines, embedded passwords, tokens, or other secrets may be exposed through process listings or logs. Do not use this example to inspect credential-bearing commands in such environments. Switching to private stdin requires a verified, supported Guard command-input contract and new tests; it is not implemented here.

Only the GitAgent tool named `cli` is checked. Read, write, MCP, plugin tools and any other execution surfaces are **not** automatically covered by this integration. An explicit allow for an unrelated tool simply leaves that tool to other GitAgent hooks; it is not a Guard authorization.

GitAgent's host hook runner logs thrown hook errors and continues, so this plugin returns explicit `block` results for subprocess, parsing, and unexpected errors from its own registered callback. A failure in GitAgent's hook runner before or outside that callback may still result in unguarded execution. The host's `runHooks` implementation also returns immediately when **any** `pre_tool_use` hook returns `modify`: a preceding modifier can skip this Guard callback entirely, and a subsequent modifier can change the command **after** Guard inspected it. Avoid combining this opt-in example with modifying pre-tool hooks on `cli`; neither hook order guarantees authorization of the final command. Do not treat this optional example as a universal security boundary. For native Guard policy, approvals and receipts, use a [supported harness](https://github.com/hashgraph-online/hol-guard/blob/main/docs/guard/harness-support.md).

## Verify the example

With GitAgent's dependencies installed, from the repository root:

```bash
npm run build
npm test
```

The integration's tests under `test/hol-guard-integration.test.ts` cover exact command forwarding, conflicting results, subprocess failure, output size and unrelated-tool behavior using controlled fixtures. These are not a live managed Guard runtime proof or independent maintainer acceptance. Validate an allowed benign command and a denied command with a harmless sentinel before experimenting with real tool execution.
