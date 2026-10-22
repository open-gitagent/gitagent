"""gitagent for Python: drives the gitagent binary over its IPC protocol.

    from gitagent import query, tool

    q = query("Where is order 42?", dir="agents/lyra")
    for message in q:
        if message["type"] == "assistant" and message["content"]:
            print(message["content"])

The agent itself runs in the gitagent binary (``GITAGENT_BIN``, or
``gitagent`` on the PATH); this module only translates calls and messages.
A query is one session: ``query("...")`` runs one turn, ``query(["a",
"b"])`` runs a turn per item, and ``send()`` adds turns while iterating.
Messages are the protocol's own, as dicts.
"""

from __future__ import annotations

import json
import os
import queue
import shutil
import subprocess
import threading
from typing import Any, Callable, Iterable, Iterator, Optional, Union

__all__ = ["query", "tool", "Query", "GitagentError", "PROTOCOL"]

PROTOCOL = 1

Message = dict[str, Any]
HookResult = Optional[dict[str, Any]]
Hook = Callable[[dict[str, Any]], HookResult]
ToolHandler = Callable[[dict[str, Any]], Union[str, dict[str, Any], None]]
ApprovalAnswer = Union[bool, tuple[bool, str], dict[str, Any]]
Approver = Callable[[dict[str, Any]], ApprovalAnswer]

HOOK_NAMES = {
    "on_session_start": "onSessionStart",
    "pre_query": "preQuery",
    "pre_tool_use": "preToolUse",
    "post_tool_use": "postToolUse",
    "post_response": "postResponse",
    "on_error": "onError",
}


class GitagentError(RuntimeError):
    """The session could not start, or the binary could not be run."""


def tool(name: str, description: str, input_schema: dict[str, Any], handler: ToolHandler) -> dict[str, Any]:
    """A tool the agent may call, run by ``handler(args)`` in this process.

    ``handler`` returns the result text, or ``{"content": ..., "isError":
    bool}``; an exception becomes an error result for the agent.
    """
    return {
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "handler": handler,
    }


def _binary(binary: Optional[str]) -> str:
    candidate = binary or os.environ.get("GITAGENT_BIN") or shutil.which("gitagent")
    if not candidate:
        raise GitagentError(
            "the gitagent binary was not found: set GITAGENT_BIN or put gitagent on the PATH"
        )
    return candidate


def query(
    prompt: Union[str, Iterable[str]],
    *,
    dir: Optional[str] = None,
    model: Optional[str] = None,
    env: Optional[str] = None,
    system_prompt: Optional[str] = None,
    system_prompt_suffix: Optional[str] = None,
    replace_builtin_tools: bool = False,
    allowed_tools: Optional[list[str]] = None,
    disallowed_tools: Optional[list[str]] = None,
    max_turns: Optional[int] = None,
    timeout_ms: Optional[int] = None,
    session_id: Optional[str] = None,
    resume: bool = False,
    mcp_servers: Optional[dict[str, Any]] = None,
    constraints: Optional[dict[str, Any]] = None,
    tools: Optional[list[dict[str, Any]]] = None,
    hooks: Optional[dict[str, Hook]] = None,
    fail_open_hooks: Iterable[str] = (),
    approver: Optional[Approver] = None,
    callback_timeout_ms: Optional[int] = None,
    conversation: bool = True,
    tool_names: Optional[str] = None,
    prices: Optional[dict[str, float]] = None,
    out_dir: Optional[str] = None,
    binary: Optional[str] = None,
) -> "Query":
    """Starts a session and returns the ``Query`` that streams its messages.

    ``hooks`` maps ``on_session_start``, ``pre_query``, ``pre_tool_use``,
    ``post_tool_use``, ``post_response`` and ``on_error`` to callables that
    get the protocol's input and return ``{"action": "allow" | "block" |
    "modify", ...}`` (or ``None`` to allow). A hook that raises blocks,
    unless its name is in ``fail_open_hooks``. ``approver(request)`` answers
    a compliance approval with ``True``/``False``, ``(ok, reason)`` or a
    dict; without one, every request is denied.
    """
    options: dict[str, Any] = {}
    for key, value in [
        ("dir", dir),
        ("model", model),
        ("env", env),
        ("systemPrompt", system_prompt),
        ("systemPromptSuffix", system_prompt_suffix),
        ("allowedTools", allowed_tools),
        ("disallowedTools", disallowed_tools),
        ("maxTurns", max_turns),
        ("timeoutMs", timeout_ms),
        ("sessionId", session_id),
        ("mcpServers", mcp_servers),
        ("constraints", constraints),
        ("callbackTimeoutMs", callback_timeout_ms),
        ("toolNames", tool_names),
        ("prices", prices),
    ]:
        if value is not None:
            options[key] = value
    if replace_builtin_tools:
        options["replaceBuiltinTools"] = True
    if resume:
        options["resume"] = True
    options["conversation"] = conversation
    hooks = hooks or {}
    callbacks: list[Any] = []
    hook_handlers: dict[str, Hook] = {}
    fail_open = set(fail_open_hooks)
    for name, handler in hooks.items():
        wire = HOOK_NAMES.get(name)
        if wire is None:
            raise GitagentError(f"unknown hook {name!r}; hooks are {', '.join(HOOK_NAMES)}")
        hook_handlers[wire] = handler
        callbacks.append({"name": wire, "failOpen": name in fail_open} if name in fail_open else wire)
    if approver is not None:
        callbacks.append("approval")
    tool_handlers = {t["name"]: t["handler"] for t in (tools or [])}
    tool_specs = [
        {"name": t["name"], "description": t["description"], "inputSchema": t["inputSchema"]}
        for t in (tools or [])
    ]
    prompts = [prompt] if isinstance(prompt, str) else list(prompt)
    return Query(
        binary=_binary(binary),
        out_dir=out_dir,
        init={
            "type": "initialize",
            "protocol": PROTOCOL,
            "options": options,
            "callbacks": callbacks,
            "tools": tool_specs,
        },
        prompts=prompts,
        hook_handlers=hook_handlers,
        tool_handlers=tool_handlers,
        approver=approver,
    )


class Query:
    """A running session: iterate it for the messages, in order.

    The iteration ends with the ``session_end`` message. Callbacks (hooks,
    tools, approvals) run on the reader thread while you iterate.
    """

    def __init__(
        self,
        *,
        binary: str,
        out_dir: Optional[str],
        init: dict[str, Any],
        prompts: list[str],
        hook_handlers: dict[str, Hook],
        tool_handlers: dict[str, ToolHandler],
        approver: Optional[Approver],
    ) -> None:
        args = [binary, "--ipc"]
        if out_dir:
            args += ["--out-dir", out_dir]
        try:
            self._proc = subprocess.Popen(
                args,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                bufsize=1,
            )
        except OSError as e:
            raise GitagentError(f"cannot start {binary}: {e}") from e
        self._hooks = hook_handlers
        self._tools = tool_handlers
        self._approver = approver
        self._pending = list(prompts)
        self._lock = threading.Lock()
        self._queue: "queue.Queue[Optional[Message]]" = queue.Queue()
        self._messages: list[Message] = []
        self._ready: Optional[Message] = None
        self._ended = False
        self._session_end: Optional[Message] = None
        self._last_turn: Optional[Message] = None
        self._open_turn = False
        # A turn ended and nothing is queued: the session ends when the
        # caller asks for the next message without sending one.
        self._idle = False
        self._ready_event = threading.Event()
        self._stderr_lines: list[str] = []
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._stderr_reader = threading.Thread(target=self._read_stderr, daemon=True)
        self._reader.start()
        self._stderr_reader.start()
        self._write(init)
        self._ready_event.wait()
        if self._ready is None:
            error = next(
                (m.get("content") for m in self._messages if m.get("subtype") == "error"),
                "the session did not start",
            )
            self.close()
            raise GitagentError(str(error))
        self._next_prompt(auto_end=True)

    # ------------------------------------------------------------ messages

    def __iter__(self) -> Iterator[Message]:
        return self

    def __next__(self) -> Message:
        if self._idle and not self._ended:
            with self._lock:
                nothing_queued = not self._pending
            if nothing_queued:
                self.end()
        message = self._queue.get()
        if message is None:
            raise StopIteration
        return message

    def __enter__(self) -> "Query":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def messages(self) -> list[Message]:
        """Every message received so far."""
        with self._lock:
            return list(self._messages)

    def session_id(self) -> str:
        return str(self._ready["metadata"].get("sessionId", "")) if self._ready else ""

    def manifest(self) -> Optional[dict[str, Any]]:
        """The agent's name, version and description, when it has an agent.yaml."""
        return self._ready["metadata"].get("agent") if self._ready else None

    def tools(self) -> list[str]:
        """The tools offered to the model."""
        return list(self._ready["metadata"].get("tools", [])) if self._ready else []

    def result(self) -> Optional[dict[str, Any]]:
        """The last turn's ``turn_end`` metadata (outcome, result, usage)."""
        return self._last_turn["metadata"] if self._last_turn else None

    def costs(self) -> Optional[dict[str, Any]]:
        """The session's token and cost totals, once it has ended."""
        return self._session_end["metadata"].get("costs") if self._session_end else None

    def stderr(self) -> str:
        """What the binary wrote to stderr (logs and warnings)."""
        return "".join(self._stderr_lines)

    # ------------------------------------------------------------ control

    def send(self, text: str) -> None:
        """Another user message, run as a turn after the current ones."""
        with self._lock:
            self._pending.append(text)
            if not self._open_turn and not self._ended:
                self._open_turn = True
                self._idle = False
                self._write({"type": "user", "content": self._pending.pop(0)})

    def steer(self, text: str) -> None:
        """A message for the running turn, before its next model call."""
        self._write({"type": "steer", "content": text})

    def abort(self) -> None:
        """Stops the running turn; the session stays open."""
        self._write({"type": "abort"})

    def kill(self) -> None:
        """Stops the running turn and ends the session."""
        self._write({"type": "kill"})

    def end(self) -> None:
        """Ends the session after the current turn."""
        with self._lock:
            if not self._ended:
                self._ended = True
                self._write({"type": "end"})

    def wait(self, timeout: Optional[float] = None) -> int:
        """Waits for the binary to exit and gives its exit code."""
        return self._proc.wait(timeout=timeout)

    def close(self) -> None:
        """Ends the session and, if the binary does not exit, kills it."""
        try:
            self.end()
        except Exception:
            pass
        try:
            self._proc.wait(timeout=10)
        except Exception:
            self._proc.kill()
            self._proc.wait()
        self._reader.join(timeout=5)
        self._stderr_reader.join(timeout=5)
        for stream in (self._proc.stdin, self._proc.stdout, self._proc.stderr):
            try:
                if stream is not None:
                    stream.close()
            except OSError:
                pass

    # ------------------------------------------------------------ internals

    def _write(self, message: dict[str, Any]) -> None:
        stdin = self._proc.stdin
        if stdin is None or stdin.closed:
            return
        try:
            stdin.write(json.dumps(message) + "\n")
            stdin.flush()
        except (BrokenPipeError, ValueError, OSError):
            pass

    def _next_prompt(self, auto_end: bool) -> None:
        with self._lock:
            if self._pending and not self._ended:
                self._open_turn = True
                self._idle = False
                self._write({"type": "user", "content": self._pending.pop(0)})
            elif auto_end and not self._ended:
                self._ended = True
                self._write({"type": "end"})
            else:
                self._idle = True

    def _read_stderr(self) -> None:
        stream = self._proc.stderr
        if stream is None:
            return
        try:
            for line in stream:
                self._stderr_lines.append(line)
        except ValueError:
            pass

    def _read(self) -> None:
        stream = self._proc.stdout
        try:
            for line in stream or []:
                if line is None:
                    break
                line = line.strip()
                if not line:
                    continue
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                self._handle(message)
        except ValueError:
            pass  # the pipe was closed under the reader
        finally:
            self._ready_event.set()
            self._queue.put(None)

    def _handle(self, message: Message) -> None:
        kind = message.get("type")
        subtype = message.get("subtype")
        if kind == "callback_request":
            self._write(
                {"type": "callback_response", "id": message["id"], "result": self._answer(message)}
            )
            return
        with self._lock:
            self._messages.append(message)
        if kind == "system" and subtype == "ready":
            self._ready = message
            self._ready_event.set()
        elif kind == "system" and subtype == "turn_end":
            self._last_turn = message
            with self._lock:
                self._open_turn = False
            self._next_prompt(auto_end=False)
        elif kind == "system" and subtype == "hook_blocked" and message.get("content", "").startswith(
            "Session blocked"
        ):
            self._ended = True
        elif kind == "system" and subtype == "session_end":
            self._session_end = message
            self._ended = True
            self._ready_event.set()
        self._queue.put(message)

    def _answer(self, request: Message) -> dict[str, Any]:
        kind = request.get("kind")
        name = request.get("name", "")
        payload = request.get("input", {})
        try:
            if kind == "hook":
                handler = self._hooks.get(name)
                result = handler(payload) if handler else None
                return result or {"action": "allow"}
            if kind == "tool":
                handler = self._tools.get(name)
                if handler is None:
                    return {"content": f"no handler for the tool {name}", "isError": True}
                result = handler(payload)
                if result is None:
                    return {"content": ""}
                if isinstance(result, dict):
                    return result
                return {"content": str(result)}
            if kind == "approval":
                if self._approver is None:
                    return {"approved": False, "reason": "no approver"}
                answer = self._approver(payload)
                if isinstance(answer, dict):
                    return answer
                if isinstance(answer, tuple):
                    return {"approved": bool(answer[0]), "reason": str(answer[1])}
                return {"approved": bool(answer)}
        except Exception as e:  # a failing handler must not take the session down
            if kind == "tool":
                return {"content": f"{type(e).__name__}: {e}", "isError": True}
            if kind == "approval":
                return {"approved": False, "reason": f"{type(e).__name__}: {e}"}
            return {"action": "block", "reason": f"{type(e).__name__}: {e}"}
        return {}
