import { createServer, type Server, type IncomingMessage, type ServerResponse } from "http";
import { WebSocketServer, WebSocket as WS } from "ws";
import { query } from "../sdk.js";
import type { VoiceServerOptions, ClientMessage, ServerMessage, MultimodalAdapter } from "./adapter.js";
import { readFileSync, readdirSync, statSync, existsSync, writeFileSync } from "fs";
import { join, dirname, resolve, relative } from "path";
import { fileURLToPath } from "url";
import { OpenAIRealtimeAdapter } from "./openai-realtime.js";
import { GeminiLiveAdapter } from "./gemini-live.js";
import { ComposioAdapter } from "../composio/index.js";
import type { GCToolDefinition } from "../sdk-types.js";

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string) => `\x1b[1m${s}\x1b[0m`;

function createAdapter(opts: VoiceServerOptions): MultimodalAdapter {
	switch (opts.adapter) {
		case "openai-realtime":
			return new OpenAIRealtimeAdapter(opts.adapterConfig);
		case "gemini-live":
			return new GeminiLiveAdapter(opts.adapterConfig);
		default:
			throw new Error(`Unknown adapter: ${opts.adapter}`);
	}
}

function loadUIHtml(): string {
	// Try dist/voice/ui.html first (built), then src/voice/ui.html (dev)
	const thisDir = dirname(fileURLToPath(import.meta.url));
	const candidates = [
		join(thisDir, "ui.html"),
		join(thisDir, "..", "..", "src", "voice", "ui.html"),
	];
	for (const path of candidates) {
		try {
			return readFileSync(path, "utf-8");
		} catch {
			// try next
		}
	}
	return "<html><body><h1>UI not found</h1><p>Run: npm run build</p></body></html>";
}

export async function startVoiceServer(opts: VoiceServerOptions): Promise<() => Promise<void>> {
	const port = opts.port || 3333;
	const uiHtml = loadUIHtml();

	// Creates a per-connection tool handler that can stream events to the browser
	function createToolHandler(sendToBrowser: (msg: ServerMessage) => void) {
		return async (prompt: string): Promise<string> => {
			let composioTools: GCToolDefinition[] = [];
			let connectedSlugs: string[] = [];
			if (composioAdapter) {
				try {
					connectedSlugs = await composioAdapter.getConnectedToolkitSlugs();
					if (connectedSlugs.length > 0) {
						// Try semantic search first, fall back to all connected tools
						composioTools = await composioAdapter.getToolsForQuery(prompt);
						if (composioTools.length === 0) {
							composioTools = await composioAdapter.getTools();
						}
						console.log(dim(`[voice] Composio: ${composioTools.length} tools injected: ${composioTools.map(t => t.name).join(", ")}`));
					}
				} catch (err: any) {
					console.error(dim(`[voice] Composio tool fetch failed: ${err.message}`));
				}
			}

			// Build system prompt suffix: always tell the agent about connected services
			let systemPromptSuffix: string | undefined;
			if (connectedSlugs.length > 0) {
				const services = connectedSlugs.map((s) => s.replace(/_/g, " ")).join(", ");
				systemPromptSuffix = [
					`You are connected to the following external services via Composio: ${services}.`,
					`You CAN perform real actions on these services — read emails, send emails, check calendars, create events, manage files, etc.`,
					`Never tell the user you "can't access" or "don't have access to" these services. You have full access. Use the available tools (prefixed "composio_") to fulfill the user's request.`,
					`When the user asks to send an email, use the SEND_EMAIL action directly — do NOT create a draft unless they explicitly ask for a draft.`,
					`Prefer direct actions over multi-step workflows.`,
				].join(" ");
			}

			const result = query({
				prompt,
				dir: opts.agentDir,
				model: opts.model,
				env: opts.env,
				...(composioTools.length ? { tools: composioTools } : {}),
				...(systemPromptSuffix ? { systemPromptSuffix } : {}),
			});

			let text = "";
			const toolResults: string[] = [];
			const errors: string[] = [];

			for await (const msg of result) {
				if (msg.type === "assistant" && msg.content) {
					text += msg.content;
				} else if (msg.type === "tool_use") {
					sendToBrowser({ type: "tool_call", toolName: msg.toolName, args: msg.args });
					console.log(dim(`[voice] Tool call: ${msg.toolName}(${JSON.stringify(msg.args).slice(0, 80)})`));
				} else if (msg.type === "tool_result") {
					sendToBrowser({ type: "tool_result", toolName: msg.toolName, content: msg.content, isError: msg.isError });
					if (msg.content) toolResults.push(msg.content);
					console.log(dim(`[voice] Tool ${msg.toolName}: ${msg.content.slice(0, 100)}${msg.content.length > 100 ? "..." : ""}`));
				} else if (msg.type === "system" && msg.subtype === "error") {
					errors.push(msg.content);
					console.error(dim(`[voice] Agent error: ${msg.content}`));
				} else if (msg.type === "delta" && msg.deltaType === "thinking") {
					sendToBrowser({ type: "agent_thinking", text: msg.content });
				}
			}

			if (text) return text;
			if (errors.length > 0) return `Error: ${errors.join("; ")}`;
			if (toolResults.length > 0) return toolResults.join("\n");
			return "(no response)";
		};
	}

	// ── File API helpers ────────────────────────────────────────────────
	const HIDDEN_DIRS = new Set([".git", "node_modules", ".gitagent", "dist", ".next", "__pycache__", ".venv"]);
	const agentRoot = resolve(opts.agentDir);

	// ── Composio integration (optional) ────────────────────────────────
	let composioAdapter: ComposioAdapter | null = null;
	if (process.env.COMPOSIO_API_KEY) {
		composioAdapter = new ComposioAdapter({
			apiKey: process.env.COMPOSIO_API_KEY,
			userId: process.env.COMPOSIO_USER_ID || "default",
		});
		console.log(dim("[voice] Composio integration enabled"));
	}

	/** Resolve and validate a requested path stays within agentDir */
	function safePath(reqPath: string): string | null {
		const abs = resolve(agentRoot, reqPath);
		if (!abs.startsWith(agentRoot)) return null;
		return abs;
	}

	interface FileEntry {
		name: string;
		path: string;
		type: "file" | "directory";
		children?: FileEntry[];
	}

	function listDir(dirPath: string, depth: number): FileEntry[] {
		if (depth > 4) return [];
		try {
			const entries = readdirSync(dirPath);
			const result: FileEntry[] = [];
			for (const name of entries) {
				if (name.startsWith(".") && HIDDEN_DIRS.has(name)) continue;
				if (HIDDEN_DIRS.has(name)) continue;
				const fullPath = join(dirPath, name);
				const relPath = relative(agentRoot, fullPath);
				try {
					const st = statSync(fullPath);
					if (st.isDirectory()) {
						result.push({
							name,
							path: relPath,
							type: "directory",
							children: listDir(fullPath, depth + 1),
						});
					} else if (st.isFile()) {
						result.push({ name, path: relPath, type: "file" });
					}
				} catch {
					// skip unreadable entries
				}
			}
			// Sort: directories first, then alphabetical
			result.sort((a, b) => {
				if (a.type !== b.type) return a.type === "directory" ? -1 : 1;
				return a.name.localeCompare(b.name);
			});
			return result;
		} catch {
			return [];
		}
	}

	function readBody(req: IncomingMessage): Promise<string> {
		return new Promise((res, rej) => {
			let body = "";
			req.on("data", (c: Buffer) => { body += c.toString(); });
			req.on("end", () => res(body));
			req.on("error", rej);
		});
	}

	function jsonReply(res: ServerResponse, status: number, data: any) {
		res.writeHead(status, { "Content-Type": "application/json" });
		res.end(JSON.stringify(data));
	}

	// HTTP server
	const httpServer: Server = createServer(async (req, res) => {
		res.setHeader("Access-Control-Allow-Origin", "*");
		res.setHeader("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS");
		res.setHeader("Access-Control-Allow-Headers", "Content-Type");

		if (req.method === "OPTIONS") {
			res.writeHead(204);
			return res.end();
		}

		const url = new URL(req.url || "/", `http://localhost:${port}`);

		if (url.pathname === "/health") {
			jsonReply(res, 200, { status: "ok" });

		} else if (url.pathname === "/" || url.pathname === "/test") {
			res.writeHead(200, { "Content-Type": "text/html" });
			res.end(uiHtml);

		} else if (url.pathname === "/api/files" && req.method === "GET") {
			// List files as a tree
			const reqPath = url.searchParams.get("path") || ".";
			const abs = safePath(reqPath);
			if (!abs) return jsonReply(res, 403, { error: "Path outside workspace" });
			const tree = listDir(abs, 0);
			jsonReply(res, 200, { root: relative(agentRoot, abs) || ".", entries: tree });

		} else if (url.pathname === "/api/file" && req.method === "GET") {
			// Read a file
			const reqPath = url.searchParams.get("path");
			if (!reqPath) return jsonReply(res, 400, { error: "Missing path param" });
			const abs = safePath(reqPath);
			if (!abs) return jsonReply(res, 403, { error: "Path outside workspace" });
			if (!existsSync(abs)) return jsonReply(res, 404, { error: "File not found" });
			try {
				const st = statSync(abs);
				if (st.size > 1024 * 1024) return jsonReply(res, 413, { error: "File too large (>1MB)" });
				const content = readFileSync(abs, "utf-8");
				jsonReply(res, 200, { path: reqPath, content });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/file" && req.method === "PUT") {
			// Write a file
			const body = await readBody(req);
			let parsed: { path: string; content: string };
			try {
				parsed = JSON.parse(body);
			} catch {
				return jsonReply(res, 400, { error: "Invalid JSON body" });
			}
			if (!parsed.path || parsed.content === undefined) return jsonReply(res, 400, { error: "Missing path or content" });
			const abs = safePath(parsed.path);
			if (!abs) return jsonReply(res, 403, { error: "Path outside workspace" });
			try {
				writeFileSync(abs, parsed.content, "utf-8");
				jsonReply(res, 200, { ok: true, path: parsed.path });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		// ── Composio OAuth callback ─────────────────────────────────────
		} else if (url.pathname === "/api/composio/callback") {
			// OAuth popup lands here after Composio processes the auth code.
			// Send a message to the opener window and close the popup.
			res.writeHead(200, { "Content-Type": "text/html" });
			res.end(`<!DOCTYPE html><html><body><script>
				if(window.opener){window.opener.postMessage({type:'composio_auth_complete'},'*');}
				window.close();
				</script><p>Authentication complete. You can close this window.</p></body></html>`);

		// ── Composio API routes ─────────────────────────────────────────
		} else if (url.pathname === "/api/composio/toolkits" && req.method === "GET") {
			if (!composioAdapter) return jsonReply(res, 501, { error: "Composio not configured" });
			try {
				const toolkits = await composioAdapter.getToolkits();
				jsonReply(res, 200, toolkits);
			} catch (err: any) {
				jsonReply(res, 502, { error: err.message });
			}

		} else if (url.pathname === "/api/composio/connect" && req.method === "POST") {
			if (!composioAdapter) return jsonReply(res, 501, { error: "Composio not configured" });
			const body = await readBody(req);
			let parsed: { toolkit: string; redirectUrl?: string };
			try { parsed = JSON.parse(body); } catch { return jsonReply(res, 400, { error: "Invalid JSON" }); }
			if (!parsed.toolkit) return jsonReply(res, 400, { error: "Missing toolkit" });
			try {
				const result = await composioAdapter.connect(parsed.toolkit, parsed.redirectUrl);
				jsonReply(res, 200, result);
			} catch (err: any) {
				jsonReply(res, 502, { error: err.message });
			}

		} else if (url.pathname === "/api/composio/connections" && req.method === "GET") {
			if (!composioAdapter) return jsonReply(res, 501, { error: "Composio not configured" });
			try {
				const connections = await composioAdapter.getConnections();
				jsonReply(res, 200, connections);
			} catch (err: any) {
				jsonReply(res, 502, { error: err.message });
			}

		} else if (url.pathname.match(/^\/api\/composio\/connections\/[^/]+$/) && req.method === "DELETE") {
			if (!composioAdapter) return jsonReply(res, 501, { error: "Composio not configured" });
			const connId = url.pathname.split("/").pop()!;
			try {
				await composioAdapter.disconnect(connId);
				jsonReply(res, 200, { ok: true });
			} catch (err: any) {
				jsonReply(res, 502, { error: err.message });
			}

		} else {
			res.writeHead(404);
			res.end();
		}
	});

	// WebSocket server — adapter-agnostic proxy
	const wss = new WebSocketServer({ server: httpServer });

	wss.on("connection", async (browserWs: WS) => {
		console.log(dim("[voice] Browser connected"));

		const adapter = createAdapter(opts);
		const sendToBrowser = (msg: ServerMessage) => safeSend(browserWs, JSON.stringify(msg));

		try {
			await adapter.connect({
				toolHandler: createToolHandler(sendToBrowser),
				onMessage: sendToBrowser,
			});
			console.log(dim(`[voice] Adapter ready (${opts.adapter})`));
		} catch (err: any) {
			console.error(dim(`[voice] Adapter connection failed: ${err.message}`));
			safeSend(browserWs, JSON.stringify({ type: "error", message: `Adapter failed: ${err.message}` }));
			browserWs.close();
			return;
		}

		// Parse browser messages into ClientMessage and forward to adapter
		browserWs.on("message", (data) => {
			try {
				const msg = JSON.parse(data.toString()) as ClientMessage;
				adapter.send(msg);
			} catch {
				// Ignore unparseable messages
			}
		});

		browserWs.on("close", () => {
			console.log(dim("[voice] Browser disconnected"));
			adapter.disconnect().catch(() => {});
		});
	});

	await new Promise<void>((resolve) => {
		httpServer.listen(port, () => resolve());
	});

	console.log(bold(`Voice server running on :${port}`));
	console.log(dim(`[voice] Backend: ${opts.adapter}`));
	console.log(dim(`[voice] Open http://localhost:${port} in your browser`));

	return async () => {
		// Force-close all open WebSocket connections so the HTTP server can shut down
		for (const client of wss.clients) {
			client.terminate();
		}
		wss.close();
		await new Promise<void>((resolve) => {
			httpServer.close(() => resolve());
		});
		console.log(dim("[voice] Server stopped"));
	};
}

function safeSend(ws: WS, data: string) {
	if (ws.readyState === WS.OPEN) {
		ws.send(data);
	}
}
