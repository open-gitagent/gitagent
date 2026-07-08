import { resolve } from "path";
import { defineConfig, externalizeDepsPlugin } from "electron-vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
	main: {
		// Keep node/electron deps external (incl. @open-gitagent/gitagent, which
		// runs in-process in the main process). Emit ESM so we can import the
		// ESM-only SDK (its package "exports" has no CJS/require condition).
		plugins: [externalizeDepsPlugin()],
		build: {
			rollupOptions: {
				input: { index: resolve(__dirname, "src/main/index.ts") },
				output: { format: "es", entryFileNames: "[name].mjs" },
			},
		},
	},
	preload: {
		plugins: [externalizeDepsPlugin()],
		build: {
			rollupOptions: {
				input: { index: resolve(__dirname, "src/preload/index.ts") },
			},
		},
	},
	renderer: {
		root: resolve(__dirname, "src/renderer"),
		build: {
			rollupOptions: {
				input: { index: resolve(__dirname, "src/renderer/index.html") },
			},
		},
		plugins: [react()],
	},
});
