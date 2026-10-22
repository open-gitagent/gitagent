# gitagent for TypeScript

Drives the gitagent binary from Node. The agent runs in the binary
(`GITAGENT_BIN`, or `gitagent` on the PATH); this package only translates
calls and messages over the IPC protocol (`gitagent --ipc`).

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
  `model`, `env`, `systemPrompt`, `systemPromptSuffix`, `allowedTools`,
  `disallowedTools`, `maxTurns`, `timeoutMs`, `sessionId`, `resume`,
  `mcpServers`, `constraints`, `conversation`, `toolNames: "v1"`, `prices`,
  `outDir`, `binary`, `abortController`.
- `for await` over the `Query` gives the messages in order (`ready`,
  `session_start`, `user`, `assistant`, `tool_use`, `tool_result`,
  `turn_end`, …, `session_end`); breaking out of the loop aborts the turn
  and ends the session.
- `send(text)` adds a turn, `steer(text)` speaks to the running turn,
  `abort()` stops it, `kill()` stops it and ends the session, `end()` ends
  the session after the current turn.
- `sessionId()`, `manifest()`, `messages()`, `result()` and `costs()`
  report on the session.
- Hooks (`onSessionStart`, `preQuery`, `preToolUse`, `postToolUse`,
  `postResponse`, `onError`) get the protocol's input and return `{action:
  "allow" | "block" | "modify", …}`; one that throws blocks, unless named in
  `failOpenHooks`. `approver(request)` answers compliance approvals; without
  one, every request is denied.

Build: `npm install && npm run build`. Type check: `npm run check`.
