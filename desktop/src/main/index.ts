import { app, BrowserWindow, shell } from "electron";
import { join, dirname } from "path";
import { fileURLToPath } from "url";
import { homedir } from "os";
import { readFileSync, existsSync } from "fs";
import { registerIpc } from "./ipc";

// ESM main has no __dirname.
const __dirname = dirname(fileURLToPath(import.meta.url));

// Load ~/.gitagent/.env into process.env so the in-process SDK picks up API
// keys + GITAGENT_MODEL_BASE_URL (the CLI does this too; the app must as well).
function loadGlobalEnv(): void {
	const p = join(homedir(), ".gitagent", ".env");
	if (!existsSync(p)) return;
	for (const raw of readFileSync(p, "utf-8").split("\n")) {
		const line = raw.trim();
		if (!line || line.startsWith("#")) continue;
		const eq = line.indexOf("=");
		if (eq <= 0) continue;
		const key = line.slice(0, eq).trim();
		let val = line.slice(eq + 1).trim();
		if ((val.startsWith('"') && val.endsWith('"')) || (val.startsWith("'") && val.endsWith("'"))) {
			val = val.slice(1, -1);
		}
		process.env[key] = val; // agent .env wins over inherited placeholders
	}
}

function createWindow(): void {
	const win = new BrowserWindow({
		width: 1120,
		height: 800,
		minWidth: 720,
		minHeight: 520,
		show: false,
		titleBarStyle: "hiddenInset",
		webPreferences: {
			preload: join(__dirname, "../preload/index.js"),
			// SDK runs in the main process; preload only bridges IPC. sandbox:false
			// is required for the preload to use ipcRenderer under contextIsolation.
			sandbox: false,
			contextIsolation: true,
		},
	});

	win.on("ready-to-show", () => win.show());
	win.webContents.setWindowOpenHandler(({ url }) => {
		void shell.openExternal(url);
		return { action: "deny" };
	});

	// electron-vite sets ELECTRON_RENDERER_URL in dev; load the built file in prod.
	if (process.env.ELECTRON_RENDERER_URL) {
		void win.loadURL(process.env.ELECTRON_RENDERER_URL);
	} else {
		void win.loadFile(join(__dirname, "../renderer/index.html"));
	}

	registerIpc(win);
}

app.whenReady().then(() => {
	loadGlobalEnv();
	createWindow();
	app.on("activate", () => {
		if (BrowserWindow.getAllWindows().length === 0) createWindow();
	});
});

app.on("window-all-closed", () => {
	if (process.platform !== "darwin") app.quit();
});
