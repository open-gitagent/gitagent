import { ipcMain, dialog, type BrowserWindow } from "electron";
import * as runner from "./agent-runner";
import * as sessions from "./sessions";
import * as artifacts from "./artifacts";
import { getSettings, saveSettings } from "./settings";
import type { StartSessionOptions, AppSettings, PermissionReply, PermissionMode } from "../shared/types";

export function registerIpc(win: BrowserWindow): void {
	ipcMain.handle("dialog:pickFolder", async () => {
		const r = await dialog.showOpenDialog(win, { properties: ["openDirectory", "createDirectory"] });
		return r.canceled || !r.filePaths[0] ? null : r.filePaths[0];
	});

	// Sessions
	ipcMain.handle("sessions:list", () => sessions.list());
	ipcMain.handle("sessions:create", (_e, opts: StartSessionOptions) => runner.createSession(win.webContents, opts));
	ipcMain.handle("sessions:transcript", (_e, id: string) => sessions.loadTranscript(id));
	ipcMain.handle("sessions:rename", (_e, id: string, title: string) => sessions.update(id, { title }));
	ipcMain.handle("sessions:delete", (_e, id: string) => sessions.remove(id));

	// Active run
	ipcMain.handle("session:send", (_e, id: string, text: string) => runner.send(win.webContents, id, text));
	ipcMain.handle("session:abort", () => runner.abort());
	ipcMain.handle("plan:approve", (_e, mode?: PermissionMode) => runner.approvePlan(mode));
	ipcMain.handle("plan:reject", (_e, feedback: string) => runner.rejectPlan(feedback));
	ipcMain.handle("permission:resolve", (_e, id: string, reply: PermissionReply) =>
		runner.resolvePermission(id, reply),
	);

	// Artifacts
	ipcMain.handle("artifacts:list", (_e, id: string) => {
		const s = sessions.get(id);
		return s ? artifacts.listArtifacts(id, s.dir) : [];
	});
	ipcMain.handle("artifacts:read", (_e, id: string, relPath: string) => {
		const s = sessions.get(id);
		if (!s) throw new Error("unknown session");
		return artifacts.readArtifact(s.dir, relPath);
	});
	ipcMain.handle("artifacts:open", (_e, id: string, relPath: string) => {
		const s = sessions.get(id);
		if (s) artifacts.openArtifact(s.dir, relPath);
	});

	// Settings
	ipcMain.handle("settings:get", () => getSettings());
	ipcMain.handle("settings:save", (_e, s: AppSettings) => saveSettings(s));
}
