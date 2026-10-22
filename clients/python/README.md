# gitagent for Python

Drives the gitagent binary from Python. The agent runs in the binary
(`GITAGENT_BIN`, or `gitagent` on the PATH); this package only translates
calls and messages over the IPC protocol (`gitagent --ipc`).

```python
from gitagent import query, tool

def lookup_order(args):
    return f"Order {args['id']} shipped on Monday."

q = query(
    "Where is order 42?",
    dir="agents/lyra",
    model="anthropic:claude-sonnet-5-5",
    tools=[tool("lookup_order", "Looks up an order.", {"properties": {"id": {"type": "string"}}}, lookup_order)],
    hooks={"pre_tool_use": lambda ctx: {"action": "allow"}},
    approver=lambda request: input(f"approve {request['toolName']}? [y/N] ") == "y",
)
for message in q:
    if message["type"] == "assistant" and message["content"]:
        print(message["content"])
print(q.result()["outcome"], q.costs())
```

- `query(prompt, **options)` starts a session; `prompt` is one message or a
  list of them (one turn each). Options are the protocol's, in snake_case:
  `dir`, `model`, `env`, `system_prompt`, `system_prompt_suffix`,
  `allowed_tools`, `disallowed_tools`, `max_turns`, `timeout_ms`,
  `session_id`, `resume`, `mcp_servers`, `constraints`, `conversation`,
  `tool_names="v1"`, `prices`, `out_dir`, `binary`.
- Iterating the `Query` gives the messages in order (`ready`,
  `session_start`, `user`, `assistant`, `tool_use`, `tool_result`,
  `turn_end`, …, `session_end`).
- `send(text)` adds a turn, `steer(text)` speaks to the running turn,
  `abort()` stops it, `kill()` stops it and ends the session, `end()` ends
  the session after the current turn, `close()` makes sure the binary exits.
- `session_id()`, `manifest()`, `tools()`, `messages()`, `result()` and
  `costs()` report on the session.
- Hooks: `hooks={"pre_tool_use": fn, ...}`, where `fn(input)` returns
  `{"action": "allow" | "block" | "modify", ...}` or `None`. A hook that
  raises blocks, unless named in `fail_open_hooks`.
- Approvals: `approver(request)` returns `True`, `False`, `(ok, reason)` or
  a dict; without one, every request is denied.

Tests: `GITAGENT_BIN=target/debug/gitagent python3 -m unittest discover -s clients/python/tests`.
