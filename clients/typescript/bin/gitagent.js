#!/usr/bin/env node
// The `gitagent` command of the npm package: runs the binary installed for
// this platform with the same arguments, and exits as it does. 1.x's remote
// sandbox (--sandbox, -s, --sandbox-repo, --sandbox-token) runs here instead.

import { spawn } from "node:child_process";
import { findBinary, PLATFORM_PACKAGES } from "../dist/binary.js";

const argv = process.argv.slice(2);
const SANDBOX_FLAGS = ["-s", "--sandbox", "--sandbox-repo", "--sandbox-token"]; // as in src/sandbox-cli.ts
if (argv.some((a) => SANDBOX_FLAGS.includes(a.split("=")[0]))) {
  const { runSandbox } = await import("../dist/sandbox-cli.js");
  process.exit(await runSandbox(argv));
}

const binary = findBinary();
if (!binary) {
  const platform = `${process.platform}-${process.arch}`;
  const pkg = PLATFORM_PACKAGES[platform];
  console.error(
    pkg
      ? `gitagent: the binary package ${pkg} is not installed.\n` +
          "Reinstall without --omit=optional (or --no-optional), or set GITAGENT_BIN to a gitagent binary."
      : `gitagent: there is no prebuilt binary for ${platform}.\n` +
          `Prebuilt: ${Object.keys(PLATFORM_PACKAGES).join(", ")}. Build from source and set GITAGENT_BIN.`,
  );
  process.exit(2);
}

const child = spawn(binary, argv, { stdio: "inherit" });

// Ctrl-C and Ctrl-\ reach the binary directly from the terminal, so the
// launcher only waits; signals sent to the launcher alone are passed on.
for (const signal of ["SIGINT", "SIGQUIT"]) process.on(signal, () => {});
for (const signal of ["SIGTERM", "SIGHUP"]) process.on(signal, () => child.kill(signal));

child.on("error", (e) => {
  console.error(`gitagent: cannot start ${binary}: ${e.message}`);
  process.exit(2);
});
child.on("exit", (code, signal) => {
  if (signal) {
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
