import WebSocket from "ws";
import type {
	MultimodalAdapter,
	MultimodalAdapterConfig,
	ClientMessage,
	ServerMessage,
} from "./adapter.js";

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;

export class OpenAIRealtimeAdapter implements MultimodalAdapter {
	private ws: WebSocket | null = null;
	private config: MultimodalAdapterConfig;
	private latestVideoFrame: { frame: string; mimeType: string } | null = null;
	private onMessage: ((msg: ServerMessage) => void) | null = null;
	private toolHandler: ((query: string) => Promise<string>) | null = null;

	constructor(config: MultimodalAdapterConfig) {
		this.config = config;
	}

	async connect(opts: {
		toolHandler: (query: string) => Promise<string>;
		onMessage: (msg: ServerMessage) => void;
	}): Promise<void> {
		this.onMessage = opts.onMessage;
		this.toolHandler = opts.toolHandler;

		const model = this.config.model || "gpt-realtime";
		const url = `wss://api.openai.com/v1/realtime?model=${model}`;

		return new Promise((resolve, reject) => {
			this.ws = new WebSocket(url, {
				headers: {
					Authorization: `Bearer ${this.config.apiKey}`,
					"OpenAI-Beta": "realtime=v1",
				},
			});

			this.ws.on("open", () => {
				this.sendSessionUpdate();
				resolve();
			});

			this.ws.on("error", (err) => {
				if (!this.ws) {
					reject(err);
				} else {
					console.error(dim(`[voice] WebSocket error: ${err.message}`));
					this.emit({ type: "error", message: err.message });
				}
			});

			this.ws.on("close", () => {
				console.log(dim("[voice] WebSocket closed"));
			});

			this.ws.on("message", (data) => {
				const event = JSON.parse(data.toString());
				this.handleEvent(event);
			});
		});
	}

	send(msg: ClientMessage): void {
		switch (msg.type) {
			case "audio":
				this.sendRaw({
					type: "input_audio_buffer.append",
					audio: msg.audio,
				});
				break;

			case "video_frame":
				// OpenAI doesn't support continuous video. Store latest frame and
				// inject it as an image on the next user turn via conversation item.
				this.latestVideoFrame = { frame: msg.frame, mimeType: msg.mimeType };
				break;

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
		}
	}

	async disconnect(): Promise<void> {
		if (this.ws) {
			this.ws.close();
			this.ws = null;
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
		if (!this.latestVideoFrame) return;

		const frame = this.latestVideoFrame;
		this.latestVideoFrame = null;

		console.log(dim("[voice] Injecting video frame into conversation"));
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
		const instructions = this.config.instructions ||
			"You are a voice interface for GitClaw, a powerful AI agent with access to the terminal, file system, and git. " +
			"You MUST use the run_agent tool for ANY request that involves doing something — running commands, opening apps, reading files, writing code, searching, browsing, installing packages, git operations, or anything actionable. " +
			"Only respond directly for simple greetings, clarifying questions, or when the user explicitly asks YOU a question. " +
			"IMPORTANT: Before calling run_agent, ALWAYS first speak a short acknowledgment to the user so they know you're working on it. " +
			"For example: 'On it, creating that now.' or 'Sure, let me do that for you.' or 'Working on it.' — then call the tool. " +
			"Never silently call the tool without saying something first. " +
			"After the tool finishes, summarize the result concisely in 1-2 sentences.";

		this.sendRaw({
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
				tools: [
					{
						type: "function",
						name: "run_agent",
						description: "Execute any request through the gitclaw agent. It has full access to the terminal (can run any shell command, open apps, install packages), file system (read/write/create files), git operations, and persistent memory. Use this for ALL actionable requests.",
						parameters: {
							type: "object",
							properties: {
								query: {
									type: "string",
									description: "The user's request to pass to the gitclaw agent",
								},
							},
							required: ["query"],
						},
					},
				],
			},
		});
	}

	private handleEvent(event: any): void {
		switch (event.type) {
			case "session.created":
				console.log(dim("[voice] Session created"));
				break;

			case "session.updated":
				console.log(dim("[voice] Session configured"));
				break;

			case "input_audio_buffer.speech_stopped":
				// VAD detected end of speech — inject latest video frame before
				// OpenAI auto-creates the response, so the model can "see" it.
				this.injectVideoFrame();
				break;

			case "conversation.item.input_audio_transcription.completed":
				if (event.transcript) {
					console.log(dim(`[voice] User: ${event.transcript}`));
					this.emit({ type: "transcript", role: "user", text: event.transcript });
				}
				break;

			case "response.audio.delta":
				if (event.delta) {
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

			case "error":
				console.error(dim(`[voice] Error: ${JSON.stringify(event.error)}`));
				this.emit({ type: "error", message: event.error?.message || "Unknown OpenAI error" });
				break;
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
