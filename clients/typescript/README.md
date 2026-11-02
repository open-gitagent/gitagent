# gitagent for TypeScript

The `gitagent` command and its TypeScript SDK.

```sh
npm install -g @open-gitagent/gitagent   # the gitagent command
npm install @open-gitagent/gitagent      # the SDK, in a project
```

npm also installs the prebuilt binary for your platform
(`@open-gitagent/gitagent-linux-x64`, `-linux-arm64`, `-darwin-arm64` or
`-darwin-x64`). The agent runs in that binary; this package translates calls
and messages over the IPC protocol (`gitagent --ipc`). To use another binary,
set `GITAGENT_BIN` or pass `binary` to `query()`.

```ts
import { query, tool } from "@open-gitagent/gitagent";

const lookupOrder = tool(
  "lookup_order",
  "Looks up an order.",
  { properties: { id: { type: "string" } } },
  async (args) => `Order ${args.id} shipped on Monday.`,
);

const q = query({
  prompt: "Where is order 42?",
  dir: "agents/lyra",
  model: "anthropic:claude-sonnet-5-5",
  tools: [lookupOrder],
  hooks: { preToolUse: (input) => ({ action: "allow" }) },
  approver: (request) => ({ approved: true }),
});
for await (const message of q) {
  if (message.type === "assistant" && message.content) console.log(message.content);
}
console.log(q.result()?.outcome, q.costs());
```

- `query(options)` starts a session; `prompt` is one message or an iterable
  of them (one turn each). The other options are the protocol's: `dir`,
  `repo` (1.x's local repo mode: `{url, token?, dir?, session?, autoCommit?}`),
  `model`, `env`, `systemPrompt`, `systemPromptSuffix`, `allowedTools`,
  `disallowedTools`, `maxTurns`, `timeoutMs`, `sessionId`, `resume`,
  `mcpServers`, `constraints`, `conversation`, `toolNames` (`"v1"`, the
  default: 1.x's tool and argument names in messages, hooks and approvals;
  `"gitagent"` for gitagent's own), `prices`, `outDir`, `binary`,
  `abortController`.
- `for await` over the `Query` gives the messages in order (`ready`,
  `session_start`, `user`, `assistant`, `tool_use`, `tool_result`,
  `turn_end`, …, `session_end`); breaking out of the loop aborts the turn
  and ends the session.
- `send(text)` adds a turn, `steer(text)` speaks to the running turn,
  `abort()` stops it, `kill()` stops it and ends the session, `end()` ends
  the session after the current turn.
- `sessionId()`, `manifest()`, `messages()`, `result()` and `costs()`
  report on the session.
- A tool's handler gets the arguments and an `AbortSignal`, aborted when
  the turn is stopped or the session ends. It returns a string, `{content,
  isError?}`, or 1.x's `{text, details?}` (the text becomes the result;
  `details` is not sent to the model).
- Hooks (`onSessionStart`, `preQuery`, `preToolUse`, `postToolUse`,
  `postToolFailure`, `fileChanged`,
  `postResponse`, `onError`) get the protocol's input and return `{action:
  "allow" | "block" | "modify", …}`; one that throws is reported on stderr
  and the action goes on (as in 1.x), unless named in `failClosedHooks`,
  which makes it block. `approver(request)` answers compliance approvals;
  without one, every request is denied.

Build: `npm install && npm run build`. Type check: `npm run check`.
