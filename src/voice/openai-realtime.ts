import WebSocket from "ws";
import {
	DEFAULT_VOICE_INSTRUCTIONS,
	type MultimodalAdapter,
	type MultimodalAdapterConfig,
	type ClientMessage,
	type ServerMessage,
} from "./adapter.js";

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;

export class OpenAIRealtimeAdapter implements MultimodalAdapter {
	private ws: WebSocket | null = null;
	private config: MultimodalAdapterConfig;
	private latestVideoFrame: { frame: string; mimeType: string } | null = null;
	private latestScreenFrame: { frame: string; mimeType: string } | null = null;
	private onMessage: ((msg: ServerMessage) => void) | null = null;
	private toolHandler: ((query: string) => Promise<string>) | null = null;
	private interrupted = false;

	// Session-refresh state
	private refreshTimer: NodeJS.Timeout | null = null;
	private refreshing = false;
	private disposed = false;
	// Refresh 5 minutes before OpenAI Realtime's 60-min hard cap
	private static readonly REFRESH_AFTER_MS = 55 * 60 * 1000;

	constructor(config: MultimodalAdapterConfig) {
		this.config = config;
	}

	async connect(opts: {
		toolHandler: (query: string) => Promise<string>;
		onMessage: (msg: ServerMessage) => void;
	}): Promise<void> {
		this.onMessage = opts.onMessage;
		this.toolHandler = opts.toolHandler;

		const model = this.config.model || "gpt-realtime-2025-08-28";
		const url = `wss://api.openai.com/v1/realtime?model=${model}`;

		// Try direct WebSocket with headers first (native Node.js / real server)
		try {
			await this.connectWs(url, {
				headers: {
					Authorization: `Bearer ${this.config.apiKey}`,
					"OpenAI-Beta": "realtime=v1",
				},
			});
			return;
		} catch (err: any) {
			const msg = err?.message || "";
			// Only retry with ephemeral token if auth failed (WebContainer drops headers)
			if (!msg.includes("authentication") && !msg.includes("401")) {
				throw err;
			}
			console.log(dim("[voice] Direct auth failed, requesting ephemeral token…"));
		}

		// Fallback: get an ephemeral session token via REST (fetch headers work everywhere)
		const keyPreview = this.config.apiKey
			? `${this.config.apiKey.slice(0, 7)}...${this.config.apiKey.slice(-4)} (${this.config.apiKey.length} chars)`
			: "(empty)";
		console.log(dim(`[voice] API key: ${keyPreview}`));
		const sessionResp = await fetch("https://api.openai.com/v1/realtime/sessions", {
			method: "POST",
			headers: {
				"Authorization": `Bearer ${this.config.apiKey}`,
				"Content-Type": "application/json",
			},
			body: JSON.stringify({ model }),
		});
		if (!sessionResp.ok) {
			const body = await sessionResp.text();
			throw new Error(`Failed to create realtime session: ${sessionResp.status} ${body}`);
		}
		const session = await sessionResp.json() as { client_secret?: { value?: string } };
		const ephemeralKey = session.client_secret?.value;
		if (!ephemeralKey) {
			throw new Error("No ephemeral key returned from realtime sessions endpoint");
		}

		await this.connectWs(url, {
			headers: {
				Authorization: `Bearer ${ephemeralKey}`,
				"OpenAI-Beta": "realtime=v1",
			},
		});
	}

	private connectWs(url: string, opts: any): Promise<void> {
		return new Promise((resolve, reject) => {
			const ws = new WebSocket(url, opts);
			let settled = false;

			ws.on("open", () => {
				// Don't resolve yet — wait for first message to confirm auth succeeded.
				// Send session.update so the server replies with session.created or error.
				this.sendSessionUpdateOn(ws);
			});

			ws.on("error", (err) => {
				if (!settled) {
					settled = true;
					ws.close();
					reject(err);
				} else {
					console.error(dim(`[voice] WebSocket error: ${err.message}`));
					this.emit({ type: "error", message: err.message });
				}
			});

			ws.on("close", () => {
				if (!settled) {
					settled = true;
					reject(new Error("WebSocket closed before open — authentication likely failed"));
				}
				console.log(dim("[voice] WebSocket closed"));
			});

			ws.on("message", (data) => {
				const event = JSON.parse(data.toString());

				// Before we've confirmed auth, check for errors
				if (!settled) {
					if (event.type === "error") {
						settled = true;
						ws.close();
						const errMsg = event.error?.message || "Unknown auth error";
						reject(new Error(errMsg));
						return;
					}
					// Any non-error message means auth succeeded
					settled = true;
					this.ws = ws;
					resolve();
				}

				this.handleEvent(event);
			});
		});
	}

	/** Send session.update on a specific ws instance (before this.ws is set). */
	private sendSessionUpdateOn(ws: WebSocket): void {
		const instructions = this.config.instructions || DEFAULT_VOICE_INSTRUCTIONS;
		const payload = {
			type: "session.update",
			session: {
				instructions,
				voice: this.config.voice || "ash",
				modalities: ["text", "audio"],
				turn_detection: {
					type: "server_vad",
					threshold: 0.6,
					prefix_padding_ms: 400,
					silence_duration_ms: 800,
					create_response: true,
				},
				input_audio_transcription: { model: "whisper-1" },
				tool_choice: "auto",
				tools: [
					{
						type: "function",
						name: "run_agent",
						description: "Your ONLY way to take action. This agent runs on the user's Mac with full shell access. It can: run ANY shell command, open apps (open -a Spotify), play music (osascript, afplay, open URLs), browse the web, read/write files, git operations, send emails, manage calendars, install packages, control system settings, and save memories. You MUST call this tool whenever the user asks you to DO anything — play music, open something, check something, build something, send something. NEVER describe an action without calling this tool. If the user asks and you just talk without calling this — you failed.",
						parameters: {
							type: "object",
							properties: {
								query: {
									type: "string",
									description: "What to do. Be specific. Include file paths for uploaded files. Examples: 'Play relaxing music on YouTube using: open https://youtube.com/...', 'Open Spotify and play chill playlist using osascript', 'Save to memory: user likes rock music'",
								},
							},
							required: ["query"],
						},
					},
				],
			},
		};
		if (ws.readyState === WebSocket.OPEN) {
			ws.send(JSON.stringify(payload));
		}
	}

	send(msg: ClientMessage): void {
		switch (msg.type) {
			case "audio":
				this.sendRaw({
					type: "input_audio_buffer.append",
					audio: msg.audio,
				});
				break;

			case "video_frame": {
				// OpenAI doesn't support continuous video. Store latest frame and
				// inject it as an image on the next user turn via conversation item.
				const source = msg.source || "camera";
				if (source === "screen") {
					this.latestScreenFrame = { frame: msg.frame, mimeType: msg.mimeType };
				} else {
					this.latestVideoFrame = { frame: msg.frame, mimeType: msg.mimeType };
				}
				break;
			}

			case "text": {
				// Send text as a user conversation item, optionally with latest video frame
				const content: any[] = [];

				if (this.latestVideoFrame) {
					content.push({
						type: "input_image",
						image_url: `data:${this.latestVideoFrame.mimeType};base64,${this.latestVideoFrame.frame}`,
					});
					this.latestVideoFrame = null;
				}

				content.push({ type: "input_text", text: msg.text });

				this.sendRaw({
					type: "conversation.item.create",
					item: {
						type: "message",
						role: "user",
						content,
					},
				});
				this.sendRaw({ type: "response.create" });
				break;
			}

			case "file": {
				const content: any[] = [];

				if (msg.mimeType.startsWith("image/")) {
					content.push({
						type: "input_image",
						image_url: `data:${msg.mimeType};base64,${msg.data}`,
					});
					content.push({ type: "input_text", text: msg.text || `[User attached image: ${msg.name}]` });
				} else {
					const decoded = Buffer.from(msg.data, "base64").toString("utf-8");
					const label = msg.text ? `${msg.text}\n\n` : "";
					content.push({ type: "input_text", text: `${label}[File: ${msg.name}]\n\`\`\`\n${decoded}\n\`\`\`` });
				}

				this.sendRaw({
					type: "conversation.item.create",
					item: { type: "message", role: "user", content },
				});
				this.sendRaw({ type: "response.create" });
				break;
			}
		}
	}

	async disconnect(): Promise<void> {
		this.disposed = true;
		if (this.refreshTimer) { clearTimeout(this.refreshTimer); this.refreshTimer = null; }
		if (this.ws) {
			this.ws.close();
			this.ws = null;
		}
	}

	/**
	 * Tear down and reopen the Realtime WS before (or right after) OpenAI's
	 * 60-minute hard cap expires. Re-sends the stored session.update so the
	 * agent picks up where it left off without the user noticing.
	 */
	private async refreshSession(reason: string): Promise<void> {
		if (this.refreshing || this.disposed) return;
		this.refreshing = true;
		console.log(dim(`[voice] Refreshing Realtime session (${reason})`));
		try {
			// Close the old WS without disposing the adapter
			if (this.refreshTimer) { clearTimeout(this.refreshTimer); this.refreshTimer = null; }
			if (this.ws) { try { this.ws.close(); } catch {} this.ws = null; }

			const model = this.config.model || "gpt-realtime-2025-08-28";
			const url = `wss://api.openai.com/v1/realtime?model=${model}`;

			try {
				await this.connectWs(url, {
					headers: {
						Authorization: `Bearer ${this.config.apiKey}`,
						"OpenAI-Beta": "realtime=v1",
					},
				});
			} catch (err: any) {
				const msg = err?.message || "";
				if (!msg.includes("authentication") && !msg.includes("401")) throw err;
				// Ephemeral token fallback (matches connect() path)
				const sessionResp = await fetch("https://api.openai.com/v1/realtime/sessions", {
					method: "POST",
					headers: { Authorization: `Bearer ${this.config.apiKey}`, "Content-Type": "application/json" },
					body: JSON.stringify({ model }),
				});
				if (!sessionResp.ok) throw new Error(`refresh ephemeral token: ${sessionResp.status}`);
				const session = (await sessionResp.json()) as { client_secret?: { value?: string } };
				const ephemeralKey = session.client_secret?.value;
				if (!ephemeralKey) throw new Error("No ephemeral key on refresh");
				await this.connectWs(url, {
					headers: { Authorization: `Bearer ${ephemeralKey}`, "OpenAI-Beta": "realtime=v1" },
				});
			}
			console.log(dim("[voice] Session refreshed"));
		} catch (err: any) {
			console.error(dim(`[voice] Session refresh failed: ${err.message}`));
			this.emit({ type: "error", message: `Voice session refresh failed: ${err.message}` });
		} finally {
			this.refreshing = false;
		}
	}

	private emit(msg: ServerMessage): void {
		this.onMessage?.(msg);
	}

	/**
	 * Inject the latest video frame as a conversation item so the model
	 * can see it when generating the next response (e.g. after a voice turn).
	 */
	private injectVideoFrame(): void {
		// Prefer screen frame over camera — it provides more useful context
		const isScreen = !!this.latestScreenFrame;
		const frame = this.latestScreenFrame || this.latestVideoFrame;
		if (!frame) return;

		// Clear both so we don't inject stale frames
		this.latestScreenFrame = null;
		this.latestVideoFrame = null;

		console.log(dim(`[voice] Injecting ${isScreen ? "screen" : "camera"} frame into conversation`));
		this.sendRaw({
			type: "conversation.item.create",
			item: {
				type: "message",
				role: "user",
				content: [{
					type: "input_image",
					image_url: `data:${frame.mimeType};base64,${frame.frame}`,
				}],
			},
		});
	}

	private sendSessionUpdate(): void {
		if (this.ws) this.sendSessionUpdateOn(this.ws);
	}

	private handleEvent(event: any): void {
		switch (event.type) {
			case "session.created":
				console.log(dim("[voice] Session created"));
				if (this.refreshTimer) clearTimeout(this.refreshTimer);
				this.refreshTimer = setTimeout(() => {
					this.refreshSession("proactive refresh before 60-min cap").catch(() => {});
				}, OpenAIRealtimeAdapter.REFRESH_AFTER_MS);
				break;

			case "session.updated":
				console.log(dim("[voice] Session configured"));
				break;

			case "input_audio_buffer.speech_started":
				// VAD detected start of speech — inject video frame (what user is looking at)
				// and cancel any in-progress response so the user can interrupt
				this.interrupted = true;
				this.injectVideoFrame();
				this.sendRaw({ type: "response.cancel" });
				this.emit({ type: "interrupt" });
				break;

			case "input_audio_buffer.speech_stopped":
				break;

			case "conversation.item.input_audio_transcription.completed":
				if (event.transcript) {
					console.log(dim(`[voice] User: ${event.transcript}`));
					this.emit({ type: "transcript", role: "user", text: event.transcript });
				}
				break;

			case "response.created":
				// New response starting — accept audio again
				this.interrupted = false;
				break;

			case "response.audio.delta":
				if (event.delta && !this.interrupted) {
					this.emit({ type: "audio_delta", audio: event.delta });
				}
				break;

			case "response.audio_transcript.delta":
				this.emit({ type: "transcript", role: "assistant", text: event.delta || "", partial: true });
				break;

			case "response.audio_transcript.done":
				if (event.transcript) {
					this.emit({ type: "transcript", role: "assistant", text: event.transcript });
				}
				break;

			case "response.function_call_arguments.done":
				this.handleFunctionCall(event);
				break;

			case "error": {
				const errMsg = event.error?.message || "Unknown OpenAI error";
				const code = event.error?.code || "";
				console.error(dim(`[voice] Error: ${JSON.stringify(event.error)}`));
				// Don't surface cancellation errors — they happen when user interrupts with no active response
				if (errMsg.toLowerCase().includes("cancellation failed")) break;
				// Session expired (60-min cap) — silently reconnect instead of surfacing
				const lower = errMsg.toLowerCase();
				if (
					lower.includes("maximum duration") ||
					lower.includes("session_expired") ||
					code === "session_expired"
				) {
					this.refreshSession("session expired").catch(() => {});
					break;
				}
				this.emit({ type: "error", message: errMsg });
				break;
			}
		}
	}

	private async handleFunctionCall(event: any): Promise<void> {
		const callId = event.call_id;
		const name = event.name;

		if (name !== "run_agent" || !this.toolHandler) {
			console.error(dim(`[voice] Unknown function call: ${name}`));
			return;
		}

		let args: { query: string };
		try {
			args = JSON.parse(event.arguments);
		} catch {
			console.error(dim("[voice] Failed to parse function arguments"));
			return;
		}

		console.log(dim(`[voice] Agent query: ${args.query}`));
		this.emit({ type: "agent_working", query: args.query });

		try {
			const result = await this.toolHandler(args.query);
			console.log(dim(`[voice] Agent response: ${result.slice(0, 200)}${result.length > 200 ? "..." : ""}`));

			this.sendRaw({
				type: "conversation.item.create",
				item: {
					type: "function_call_output",
					call_id: callId,
					output: result,
				},
			});
			this.sendRaw({ type: "response.create" });
			this.emit({ type: "agent_done", result: result.slice(0, 500) });
		} catch (err: any) {
			console.error(dim(`[voice] Agent error: ${err.message}`));
			this.sendRaw({
				type: "conversation.item.create",
				item: {
					type: "function_call_output",
					call_id: callId,
					output: `Error: ${err.message}`,
				},
			});
			this.sendRaw({ type: "response.create" });
			this.emit({ type: "error", message: err.message });
		}
	}

	private sendRaw(event: any): void {
		if (this.ws && this.ws.readyState === WebSocket.OPEN) {
			this.ws.send(JSON.stringify(event));
		}
	}
}
