// Test setup: lets node:test (with --experimental-strip-types) run the TypeScript
// sources directly. The source uses NodeNext `.js` import specifiers (required by
// tsc), but type-stripping does not rewrite them, so a bare `.js` specifier fails
// to resolve to its `.ts` sibling. This in-thread resolve hook maps `./x.js` →
// `./x.ts` when the `.ts` file exists, so tests can import from ../src/*.ts.
import { registerHooks } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

registerHooks({
	resolve(specifier, context, nextResolve) {
		if (
			(specifier.startsWith("./") || specifier.startsWith("../")) &&
			specifier.endsWith(".js")
		) {
			const tsSpecifier = specifier.slice(0, -3) + ".ts";
			try {
				const url = new URL(tsSpecifier, context.parentURL);
				if (existsSync(fileURLToPath(url))) {
					return nextResolve(tsSpecifier, context);
				}
			} catch {
				/* fall through to default resolution */
			}
		}
		return nextResolve(specifier, context);
	},
});
