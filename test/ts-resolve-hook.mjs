// ESM resolve hook: if a `.js` import inside src/ has no matching `.js`
// on disk, retry with the `.ts` extension. Lets node --experimental-strip-types
// run TypeScript sources whose internal imports follow the Node16 `.js` style.

import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

export async function resolve(specifier, context, nextResolve) {
	if (specifier.startsWith(".") && specifier.endsWith(".js")) {
		try {
			return await nextResolve(specifier, context);
		} catch (err) {
			if (err?.code !== "ERR_MODULE_NOT_FOUND") throw err;
			const tsSpecifier = specifier.slice(0, -3) + ".ts";
			try {
				const candidate = await nextResolve(tsSpecifier, context);
				if (candidate?.url?.startsWith("file://") && existsSync(fileURLToPath(candidate.url))) {
					return candidate;
				}
			} catch {
				// fall through and re-throw the original .js miss
			}
			throw err;
		}
	}
	return nextResolve(specifier, context);
}
