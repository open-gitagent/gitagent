/**
 * gitagent for TypeScript: drives the gitagent binary over its IPC protocol.
 *
 * ```ts
 * import { query, tool } from "@open-gitagent/gitagent";
 *
 * for await (const message of query({ prompt: "Where is order 42?", dir: "agents/lyra" })) {
 *   if (message.type === "assistant" && message.content) console.log(message.content);
 * }
 * ```
 *
 * The agent runs in the gitagent binary (`GITAGENT_BIN`, or `gitagent` on
 * the PATH); this package only translates calls and messages. A query is one
 * session: a string prompt runs one turn, an async iterable of strings runs
 * a turn per item, and `send()` adds turns while iterating.
 */

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createInterface } from "node:readline";

export const PROTOCOL = 1;

export type JsonObject = Record<string, unknown>;

/** A message of the protocol; `type` tells them apart. */
export interface GCMessage extends JsonObject {
  type: string;
  subtype?: string;
  content?: unknown;
  metadata?: JsonObject;
}

export interface GCHookResult {
  action: "allow" | "block" | "modify";
  reason?: string;
  args?: JsonObject;
  content?: string;
}

export type GCHook = (input: JsonObject) => GCHookResult | undefined | void | Promise<GCHookResult | undefined | void>;

export interface GCHooks {
  onSessionStart?: GCHook;
  preQuery?: GCHook;
  preToolUse?: GCHook;
  postToolUse?: GCHook;
  postResponse?: GCHook;
  onError?: GCHook;
}

export type ApprovalAnswer = boolean | { approved: boolean; reason?: string };

export interface GCToolDefinition {
  name: string;
  description: string;
  inputSchema: JsonObject;
  handler: (args: JsonObject) => unknown | Promise<unknown>;
}

export interface QueryOptions {
  prompt: string | Iterable<string> | AsyncIterable<string>;
  dir?: string;
  model?: string;
  env?: string;
  systemPrompt?: string;
  systemPromptSuffix?: string;
  replaceBuiltinTools?: boolean;
  allowedTools?: string[];
  disallowedTools?: string[];
  maxTurns?: number;
  timeoutMs?: number;
  sessionId?: string;
  resume?: boolean;
  mcpServers?: Record<string, JsonObject>;
  constraints?: { temperature?: number; maxTokens?: number };
  tools?: GCToolDefinition[];
  hooks?: GCHooks;
  /** Hooks whose failure lets the action go on. */
  failOpenHooks?: (keyof GCHooks)[];
  approver?: (request: JsonObject) => ApprovalAnswer | Promise<ApprovalAnswer>;
  callbackTimeoutMs?: number;
  /** Default true: a plain reply ends each turn. */
  conversation?: boolean;
  /** `"v1"` for gitagent 1.x tool and argument names in messages. */
  toolNames?: "v1";
  prices?: { input: number; output: number; cacheRead: number; cacheWrite: number };
  outDir?: string;
  binary?: string;
  abortController?: AbortController;
}

/** A tool the agent may call, run by `handler(args)` in this process. */
export function tool(
  name: string,
  description: string,
  inputSchema: JsonObject,
  handler: GCToolDefinition["handler"],
): GCToolDefinition {
  return { name, description, inputSchema, handler };
}

export class GitagentError extends Error {}

function binaryPath(binary?: string): string {
  return binary ?? process.env.GITAGENT_BIN ?? "gitagent";
}

/** Starts a session and returns the `Query` that streams its messages. */
export function query(options: QueryOptions): Query {
  const o = options;
  const wire: JsonObject = {};
  const copy = (key: keyof QueryOptions, name: string) => {
    if (o[key] !== undefined) wire[name] = o[key];
  };
  copy("dir", "dir");
  copy("model", "model");
  copy("env", "env");
  copy("systemPrompt", "systemPrompt");
  copy("systemPromptSuffix", "systemPromptSuffix");
  copy("replaceBuiltinTools", "replaceBuiltinTools");
  copy("allowedTools", "allowedTools");
  copy("disallowedTools", "disallowedTools");
  copy("maxTurns", "maxTurns");
  copy("timeoutMs", "timeoutMs");
  copy("sessionId", "sessionId");
  copy("resume", "resume");
  copy("mcpServers", "mcpServers");
  copy("constraints", "constraints");
  copy("callbackTimeoutMs", "callbackTimeoutMs");
  copy("toolNames", "toolNames");
  copy("prices", "prices");
  wire.conversation = o.conversation ?? true;
  const failOpen = new Set(o.failOpenHooks ?? []);
  const callbacks: unknown[] = [];
  const hooks = o.hooks ?? {};
  for (const name of Object.keys(hooks) as (keyof GCHooks)[]) {
    if (!hooks[name]) continue;
    callbacks.push(failOpen.has(name) ? { name, failOpen: true } : name);
  }
  if (o.approver) callbacks.push("approval");
  const tools = o.tools ?? [];
  const init = {
    type: "initialize",
    protocol: PROTOCOL,
    options: wire,
    callbacks,
    tools: tools.map((t) => ({ name: t.name, description: t.description, inputSchema: t.inputSchema })),
  };
  return new Query(binaryPath(o.binary), o.outDir, init, o.prompt, hooks, tools, o.approver, o.abortController);
}

/**
 * A running session: `for await` it for the messages, in order. The
 * iteration ends with the `session_end` message; breaking out of the loop
 * aborts the running turn and ends the session.
 */
export class Query implements AsyncIterable<GCMessage> {
  private readonly proc: ChildProcessWithoutNullStreams;
  private readonly hooks: GCHooks;
  private readonly tools: Map<string, GCToolDefinition>;
  private readonly approver?: QueryOptions["approver"];
  private readonly received: GCMessage[] = [];
  private readonly waiting: GCMessage[] = [];
  private readonly wakers: ((m: GCMessage | null) => void)[] = [];
  private readonly stderrLines: string[] = [];
  private readonly pending: string[] = [];
  private promptsDone = false;
  private openTurn = false;
  /** A turn ended with nothing queued: the session ends when the caller
   *  asks for the next message without sending one. */
  private idle = false;
  private ended = false;
  private closed = false;
  private ready?: GCMessage;
  private lastTurn?: GCMessage;
  private sessionEnd?: GCMessage;
  private readyResolve!: (ok: boolean) => void;
  readonly readyPromise: Promise<boolean>;

  constructor(
    binary: string,
    outDir: string | undefined,
    init: JsonObject,
    prompt: QueryOptions["prompt"],
    hooks: GCHooks,
    tools: GCToolDefinition[],
    approver: QueryOptions["approver"],
    abortController?: AbortController,
  ) {
    const args = ["--ipc"];
    if (outDir) args.push("--out-dir", outDir);
    this.proc = spawn(binary, args, { stdio: ["pipe", "pipe", "pipe"] });
    this.hooks = hooks;
    this.tools = new Map(tools.map((t) => [t.name, t]));
    this.approver = approver;
    this.readyPromise = new Promise((resolve) => {
      this.readyResolve = resolve;
    });
    this.proc.on("error", (e) => {
      this.push({ type: "system", subtype: "error", content: `cannot start ${binary}: ${e.message}` });
      this.finish();
    });
    this.proc.on("close", () => this.finish());
    createInterface({ input: this.proc.stderr }).on("line", (l) => this.stderrLines.push(l));
    createInterface({ input: this.proc.stdout }).on("line", (line) => {
      if (!line.trim()) return;
      let message: GCMessage;
      try {
        message = JSON.parse(line) as GCMessage;
      } catch {
        return;
      }
      void this.handle(message);
    });
    abortController?.signal.addEventListener("abort", () => this.abort(), { once: true });
    this.write(init);
    void this.feed(prompt);
  }

  // ------------------------------------------------------------ messages

  [Symbol.asyncIterator](): AsyncIterator<GCMessage> {
    return {
      next: async () => {
        const message = await this.next();
        return message === null ? { value: undefined, done: true } : { value: message, done: false };
      },
      return: async () => {
        this.abort();
        this.end();
        return { value: undefined, done: true };
      },
      throw: async (e: unknown) => {
        this.abort();
        this.end();
        throw e;
      },
    };
  }

  /** The next message, or `null` once the session has ended. */
  async next(): Promise<GCMessage | null> {
    if (this.idle && this.promptsDone && this.pending.length === 0) this.end();
    return this.take();
  }

  /** Every message received so far. */
  messages(): GCMessage[] {
    return [...this.received];
  }

  sessionId(): string {
    return String((this.ready?.metadata as JsonObject | undefined)?.sessionId ?? "");
  }

  /** The agent's name, version and description, when it has an agent.yaml. */
  manifest(): JsonObject | undefined {
    return (this.ready?.metadata as JsonObject | undefined)?.agent as JsonObject | undefined;
  }

  /** The last turn's `turn_end` metadata (outcome, result, usage). */
  result(): JsonObject | undefined {
    return this.lastTurn?.metadata;
  }

  /** The session's token and cost totals, once it has ended. */
  costs(): JsonObject | undefined {
    return (this.sessionEnd?.metadata as JsonObject | undefined)?.costs as JsonObject | undefined;
  }

  /** What the binary wrote to stderr (logs and warnings). */
  stderr(): string {
    return this.stderrLines.join("\n");
  }

  // ------------------------------------------------------------ control

  /** Another user message, run as a turn after the current ones. */
  send(text: string): void {
    this.pending.push(text);
    this.nextPrompt();
  }

  /** A message for the running turn, before its next model call. */
  steer(text: string): void {
    this.write({ type: "steer", content: text });
  }

  /** Stops the running turn; the session stays open. */
  abort(): void {
    this.write({ type: "abort" });
  }

  /** Stops the running turn and ends the session. */
  kill(): void {
    this.write({ type: "kill" });
  }

  /** Ends the session after the current turn. */
  end(): void {
    if (this.ended) return;
    this.ended = true;
    this.write({ type: "end" });
  }

  /** Waits for the binary to exit and gives its exit code. */
  wait(): Promise<number> {
    return new Promise((resolve) => {
      if (this.proc.exitCode !== null) return resolve(this.proc.exitCode);
      this.proc.on("close", (code) => resolve(code ?? -1));
    });
  }

  // ------------------------------------------------------------ internals

  private write(message: JsonObject): void {
    if (this.closed || !this.proc.stdin.writable) return;
    this.proc.stdin.write(JSON.stringify(message) + "\n");
  }

  private push(message: GCMessage): void {
    this.received.push(message);
    const waker = this.wakers.shift();
    if (waker) waker(message);
    else this.waiting.push(message);
  }

  private finish(): void {
    if (this.closed) return;
    this.closed = true;
    this.readyResolve(this.ready !== undefined);
    for (const w of this.wakers.splice(0)) w(null);
  }

  private take(): Promise<GCMessage | null> {
    const queued = this.waiting.shift();
    if (queued) return Promise.resolve(queued);
    if (this.closed) return Promise.resolve(null);
    return new Promise((resolve) => this.wakers.push(resolve));
  }

  private async feed(prompt: QueryOptions["prompt"]): Promise<void> {
    if (typeof prompt === "string") {
      this.pending.push(prompt);
    } else {
      for await (const p of prompt as AsyncIterable<string>) {
        this.pending.push(p);
        this.nextPrompt();
      }
    }
    this.promptsDone = true;
    this.nextPrompt();
    // No prompts at all: nothing to wait for.
    if (this.idle && this.pending.length === 0 && !this.openTurn) this.end();
  }

  private nextPrompt(): void {
    if (this.openTurn || this.ended) return;
    const text = this.pending.shift();
    if (text !== undefined) {
      this.openTurn = true;
      this.idle = false;
      this.write({ type: "user", content: text });
    } else {
      this.idle = true;
    }
  }

  private async handle(message: GCMessage): Promise<void> {
    if (message.type === "callback_request") {
      const result = await this.answer(message);
      this.write({ type: "callback_response", id: message.id, result });
      return;
    }
    if (message.type === "system" && message.subtype === "ready") {
      this.ready = message;
      this.readyResolve(true);
    } else if (message.type === "system" && message.subtype === "turn_end") {
      this.lastTurn = message;
      this.openTurn = false;
      this.nextPrompt();
    } else if (message.type === "system" && message.subtype === "session_end") {
      this.sessionEnd = message;
      this.ended = true;
    }
    this.push(message);
  }

  private async answer(request: GCMessage): Promise<JsonObject> {
    const kind = request.kind as string;
    const name = request.name as keyof GCHooks & string;
    const input = (request.input ?? {}) as JsonObject;
    try {
      if (kind === "hook") {
        const handler = this.hooks[name];
        const result = handler ? await handler(input) : undefined;
        return (result as JsonObject | undefined) ?? { action: "allow" };
      }
      if (kind === "tool") {
        const t = this.tools.get(name);
        if (!t) return { content: `no handler for the tool ${name}`, isError: true };
        const result = await t.handler(input);
        if (result === undefined || result === null) return { content: "" };
        if (typeof result === "object") return result as JsonObject;
        return { content: String(result) };
      }
      if (kind === "approval") {
        if (!this.approver) return { approved: false, reason: "no approver" };
        const answer = await this.approver(input);
        return typeof answer === "boolean" ? { approved: answer } : answer;
      }
    } catch (e) {
      const why = e instanceof Error ? e.message : String(e);
      if (kind === "tool") return { content: why, isError: true };
      if (kind === "approval") return { approved: false, reason: why };
      return { action: "block", reason: why };
    }
    return {};
  }
}
