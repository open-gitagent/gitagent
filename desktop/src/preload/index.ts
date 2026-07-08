import { contextBridge, ipcRenderer } from "electron";
import type { GitagentApi, UIEvent, PermissionRequest } from "../shared/types";

const api: GitagentApi = {
	pickFolder: () => ipcRenderer.invoke("dialog:pickFolder"),

	listSessions: () => ipcRenderer.invoke("sessions:list"),
	createSession: (opts) => ipcRenderer.invoke("sessions:create", opts),
	loadTranscript: (id) => ipcRenderer.invoke("sessions:transcript", id),
	renameSession: (id, title) => ipcRenderer.invoke("sessions:rename", id, title),
	deleteSession: (id) => ipcRenderer.invoke("sessions:delete", id),

	send: (sessionId, text) => ipcRenderer.invoke("session:send", sessionId, text),
	abort: () => ipcRenderer.invoke("session:abort"),
	approvePlan: (mode) => ipcRenderer.invoke("plan:approve", mode),
	rejectPlan: (feedback) => ipcRenderer.invoke("plan:reject", feedback),
	resolvePermission: (id, reply) => ipcRenderer.invoke("permission:resolve", id, reply),

	listArtifacts: (id) => ipcRenderer.invoke("artifacts:list", id),
	readArtifact: (id, relPath) => ipcRenderer.invoke("artifacts:read", id, relPath),
	openArtifact: (id, relPath) => ipcRenderer.invoke("artifacts:open", id, relPath),

	getSettings: () => ipcRenderer.invoke("settings:get"),
	saveSettings: (s) => ipcRenderer.invoke("settings:save", s),

	onEvent: (cb: (e: UIEvent) => void) => {
		const h = (_e: unknown, ev: UIEvent) => cb(ev);
		ipcRenderer.on("agent:event", h);
		return () => ipcRenderer.removeListener("agent:event", h);
	},
	onPermissionRequest: (cb: (r: PermissionRequest) => void) => {
		const h = (_e: unknown, r: PermissionRequest) => cb(r);
		ipcRenderer.on("permission:request", h);
		return () => ipcRenderer.removeListener("permission:request", h);
	},
};

contextBridge.exposeInMainWorld("gitagent", api);
