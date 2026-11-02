/**
 * `gitagent --sandbox`, as in 1.x: the npm command handles it here instead
 * of passing it to the binary. The agent runs on this machine; its commands
 * and file edits run in a sandbox VM with a clone of the repository, which
 * is committed when the VM stops. Without a prompt it reads one task per
 * line, like 1.x's session.
 */

import { createInterface } from "node:readline";
import { createSandboxContext, type SandboxContext } from "./sandbox.js";
import { query } from "./index.js";

/** The flags that start a sandbox; any of them sends the command here. */
export const SANDBOX_FLAGS = ["-s", "--sandbox", "--sandbox-repo", "--sandbox-token"];

interface Args {
  prompt?: string;
  dir: string;
  model?: string;
  env?: string;
  repository?: string;
  token?: string;
  session?: string;
}

const dim = (s: string) => (process.stderr.isTTY ? `\x1b[2m${s}\x1b[0m` : s);

function parse(argv: string[]): Args {
  const a: Args = { dir: process.cwd() };
  for (let i = 0; i < argv.length; i++) {
    const [flag, inline] = argv[i].startsWith("--") && argv[i].includes("=") ? argv[i].split(/=(.*)/s, 2) : [argv[i], undefined];
    const value = () => {
      const v = inline ?? argv[++i];
      if (v === undefined) throw new Error(`${flag} needs a value`);
      return v;
    };
    switch (flag) {
      case "-s":
      case "--sandbox":
        break;
      case "--sandbox-repo":
        a.repository = value();
        break;
      case "--sandbox-token":
        a.token = value();
        break;
      case "--session":
        a.session = value();
        break;
      case "-p":
      case "--prompt":
        a.prompt = value();
        break;
      case "-d":
      case "--dir":
        a.dir = value();
        break;
      case "-m":
      case "--model":
        a.model = value();
        break;
      case "-e":
      case "--env":
        a.env = value();
        break;
      case "-r":
      case "--repo":
        throw new Error("--repo and --sandbox are mutually exclusive");
      default:
        if (flag.startsWith("-")) throw new Error(`${flag} cannot be used with --sandbox`);
        if (a.prompt !== undefined) throw new Error(`unexpected argument ${JSON.stringify(flag)}`);
        a.prompt = flag;
    }
  }
  return a;
}

/** Runs `gitagent --sandbox …` and gives the exit code. */
export async function runSandbox(argv: string[]): Promise<number> {
  let args: Args;
  try {
    args = parse(argv);
  } catch (e) {
    console.error(`gitagent: ${(e as Error).message}`);
    return 2;
  }

  let ctx: SandboxContext;
  try {
    ctx = await createSandboxContext(
      { repository: args.repository, token: args.token, session: args.session },
      args.dir,
    );
    console.error(dim("Starting sandbox VM..."));
    await ctx.gitMachine.start();
    console.error(dim(`Sandbox ready (repo: ${ctx.repoPath})`));
  } catch (e) {
    console.error(`gitagent: ${(e as Error).message}`);
    return 2;
  }

  const interactive = args.prompt === undefined;
  const rl = interactive ? createInterface({ input: process.stdin, output: process.stdout, prompt: "> " }) : undefined;
  async function* typed(): AsyncGenerator<string> {
    rl!.prompt();
    for await (const line of rl!) {
      const text = line.trim();
      if (["exit", "quit", "/exit", "/quit"].includes(text)) break;
      if (text) yield text;
      else rl!.prompt();
    }
  }

  const q = query({
    prompt: interactive ? typed() : args.prompt!,
    dir: args.dir,
    model: args.model,
    env: args.env,
    sandbox: ctx,
  });
  let stopping = false;
  const onInterrupt = () => {
    if (stopping) process.exit(130);
    stopping = true;
    q.abort();
  };
  process.on("SIGINT", onInterrupt);

  let code = 0;
  try {
    for await (const m of q) {
      if (m.type === "assistant" && typeof m.content === "string" && m.content) {
        console.log(m.content);
      } else if (m.type === "tool_use") {
        console.error(dim(`$ ${String(m.toolName ?? "")} ${JSON.stringify(m.args ?? {})}`));
      } else if (m.type === "system" && (m.subtype === "error" || m.subtype === "warning")) {
        console.error(`${m.subtype}: ${String(m.content ?? "")}`);
        if (m.subtype === "error") code = 3;
      } else if (m.type === "system" && m.subtype === "turn_end") {
        stopping = false;
        if (interactive) rl!.prompt();
      }
    }
  } finally {
    process.off("SIGINT", onInterrupt);
    rl?.close();
    console.error(dim("Stopping sandbox..."));
    try {
      await ctx.gitMachine.stop();
    } catch (e) {
      console.error(`gitagent: stopping the sandbox failed: ${(e as Error).message}`);
      code = code || 3;
    }
  }
  return code;
}
