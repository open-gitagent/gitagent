/**
 * Where the gitagent binary comes from. `npm install @open-gitagent/gitagent`
 * also installs the one platform package below that matches the machine,
 * which carries the prebuilt binary in `bin/gitagent`.
 */

import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";

/** The npm package with the prebuilt binary, by `${process.platform}-${process.arch}`. */
export const PLATFORM_PACKAGES: Record<string, string> = {
  "linux-x64": "@open-gitagent/gitagent-linux-x64",
  "linux-arm64": "@open-gitagent/gitagent-linux-arm64",
  "darwin-arm64": "@open-gitagent/gitagent-darwin-arm64",
  "darwin-x64": "@open-gitagent/gitagent-darwin-x64",
};

/**
 * The binary to run: `GITAGENT_BIN`, else the platform package installed
 * with this one, else the install script's `~/.local/bin/gitagent`.
 * Undefined when none of them exists.
 */
export function findBinary(): string | undefined {
  const fromEnv = process.env.GITAGENT_BIN;
  if (fromEnv) return fromEnv;
  const pkg = PLATFORM_PACKAGES[`${process.platform}-${process.arch}`];
  if (pkg) {
    try {
      const path = createRequire(import.meta.url).resolve(`${pkg}/bin/gitagent`);
      if (existsSync(path)) return path;
    } catch {
      // Not installed, for example with --omit=optional.
    }
  }
  const installed = join(homedir(), ".local", "bin", "gitagent");
  return existsSync(installed) ? installed : undefined;
}
