import WebSocket from "ws";
import type {
	MultimodalAdapter,
	MultimodalAdapterConfig,
	ClientMessage,
	ServerMessage,
} from "./adapter.js";

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;

/**
 * Downsample 24kHz PCM (Int16LE) to 16kHz by linear interpolation (2 of every 3 samples).
 * Input: base64-encoded 24kHz Int16LE. Output: base64-encoded 16kHz Int16LE.
 */
function downsample24kTo16k(base64_24k: string): string {
	const binary = Buffer.from(base64_24k, "base64");
	const samples24 = new Int16Array(binary.buffer, binary.byteOffset, binary.byteLength / 2);
	const outLength = Math.floor(samples24.length * 2 / 3);
	const samples16 = new Int16Array(outLength);

	for (let i = 0; i < outLength; i++) {
		// Map output index to fractional input index
		const srcIdx = i * 1.5;
		const lo = Math.floor(srcIdx);
		const frac = srcIdx - lo;
		const hi = Math.min(lo + 1, samples24.length - 1);
		samples16[i] = Math.round(samples24[lo] * (1 - frac) + samples24[hi] * frac);
	}

	return Buffer.from(samples16.buffer).toString("base64");
}

/**
 * Upsample 16kHz PCM (Int16LE) to 24kHz by linear interpolation.
 * Input: base64-encoded 16kHz Int16LE. Output: base64-encoded 24kHz Int16LE.
 */
function upsample16kTo24k(base64_16k: string): string {
	const binary = Buffer.from(base64_16k, "base64");
	const samples16 = new Int16Array(binary.buffer, binary.byteOffset, binary.byteLength / 2);
	const outLength = Math.floor(samples16.length * 3 / 2);
	const samples24 = new Int16Array(outLength);

	for (let i = 0; i < outLength; i++) {
		const srcIdx = i * (2 / 3);
		const lo = Math.floor(srcIdx);
		const frac = srcIdx - lo;
		const hi = Math.min(lo + 1, samples16.length - 1);
		samples24[i] = Math.round(samples16[lo] * (1 - frac) + samples16[hi] * frac);
	}

	return Buffer.from(samples24.buffer).toString("base64");
}

export class GeminiLiveAdapter implements MultimodalAdapter {
	private ws: WebSocket | null = null;
	private config: MultimodalAdapterConfig;
	private onMessage: ((msg: ServerMessage) => void) | null = null;
	private toolHandler: ((query: string) => Promise<string>) | null = null;
	private setupDone = false;

	constructor(config: MultimodalAdapterConfig) {
		this.config = config;
	}

	async connect(opts: {
		toolHandler: (query: string) => Promise<string>;
		onMessage: (msg: ServerMessage) => void;
	}): Promise<void> {
		this.onMessage = opts.onMessage;
		this.toolHandler = opts.toolHandler;
		this.setupDone = false;

		const model = this.config.model || "models/gemini-2.5-flash-native-audio-preview";
		const url = `wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=${this.config.apiKey}`;

		return new Promise((resolve, reject) => {
			this.ws = new WebSocket(url);

			this.ws.on("open", () => {
				console.log(dim("[voice] Connected to Gemini Multimodal Live"));
				this.sendSetup(model);
			});

			this.ws.on("error", (err) => {
				console.error(dim(`[voice] Gemini WS error: ${err.message}`));
				if (!this.setupDone) {
					reject(err);
				} else {
					this.emit({ type: "error", message: err.message });
				}
			});

			this.ws.on("close", () => {
				console.log(dim("[voice] Gemini WS closed"));
			});

			this.ws.on("message", (data) => {
				try {
					const msg = JSON.parse(data.toString());
					this.handleGeminiMessage(msg);

					// Resolve after setup acknowledgment
					if (!this.setupDone && msg.setupComplete) {
						this.setupDone = true;
						console.log(dim("[voice] Gemini session ready"));
						resolve();
					}
				} catch (err: any) {
					console.error(dim(`[voice] Gemini parse error: ${err.message}`));
				}
			});
		});
	}

	send(msg: ClientMessage): void {
		switch (msg.type) {
			case "audio":
				// Browser sends 24kHz, Gemini expects 16kHz
				this.sendRaw({
					realtimeInput: {
						mediaChunks: [{
							mimeType: "audio/pcm;rate=16000",
							data: downsample24kTo16k(msg.audio),
						}],
					},
				});
				break;

			case "video_frame":
				// Gemini supports continuous video streaming natively
				this.sendRaw({
					realtimeInput: {
						mediaChunks: [{
							mimeType: msg.mimeType,
							data: msg.frame,
						}],
					},
				});
				break;

			case "text":
				this.sendRaw({
					clientContent: {
						turns: [{
							role: "user",
							parts: [{ text: msg.text }],
						}],
						turnComplete: true,
					},
				});
				break;
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

	private sendSetup(model: string): void {
		const instructions = this.config.instructions ||
			"You are a voice interface for GitClaw, a powerful AI agent with access to the terminal, file system, and git. " +
			"You MUST use the run_agent tool for ANY request that involves doing something — running commands, opening apps, reading files, writing code, searching, browsing, installing packages, git operations, or anything actionable. " +
			"Only respond directly for simple greetings, clarifying questions, or when the user explicitly asks YOU a question. " +
			"IMPORTANT: Before calling run_agent, ALWAYS first speak a short acknowledgment to the user so they know you're working on it. " +
			"For example: 'On it, creating that now.' or 'Sure, let me do that for you.' or 'Working on it.' — then call the tool. " +
			"Never silently call the tool without saying something first. " +
			"After the tool finishes, summarize the result concisely in 1-2 sentences.";

		const voiceName = this.config.voice || "Aoede";

		this.sendRaw({
			setup: {
				model,
				generationConfig: {
					responseModalities: ["AUDIO", "TEXT"],
					speechConfig: {
						voiceConfig: {
							prebuiltVoiceConfig: { voiceName },
						},
					},
				},
				tools: [{
					functionDeclarations: [{
						name: "run_agent",
						description: "Execute any request through the gitclaw agent. It has full access to the terminal (can run any shell command, open apps, install packages), file system (read/write/create files), git operations, and persistent memory. Use this for ALL actionable requests.",
						parameters: {
							type: "OBJECT",
							properties: {
								query: {
									type: "STRING",
									description: "The user's request to pass to the gitclaw agent",
								},
							},
							required: ["query"],
						},
					}],
				}],
				systemInstruction: {
					parts: [{ text: instructions }],
				},
				contextWindowCompression: {
					triggerTokens: 25000,
					slidingWindow: { targetTokens: 12500 },
				},
			},
		});
	}

	private handleGeminiMessage(msg: any): void {
		// Tool calls
		if (msg.toolCall) {
			this.handleToolCall(msg.toolCall);
			return;
		}

		// Server content (audio/text responses)
		if (msg.serverContent) {
			const sc = msg.serverContent;

			// Model turn parts
			if (sc.modelTurn?.parts) {
				for (const part of sc.modelTurn.parts) {
					if (part.inlineData) {
						const mimeType: string = part.inlineData.mimeType || "";
						if (mimeType.startsWith("audio/")) {
							// Gemini outputs 16kHz, browser expects 24kHz
							const audio24k = upsample16kTo24k(part.inlineData.data);
							this.emit({ type: "audio_delta", audio: audio24k });
						}
					}
					if (part.text) {
						this.emit({
							type: "transcript",
							role: "assistant",
							text: part.text,
							partial: !sc.turnComplete,
						});
					}
				}
			}

			// Turn complete marker
			if (sc.turnComplete && sc.modelTurn?.parts) {
				const textParts = sc.modelTurn.parts.filter((p: any) => p.text).map((p: any) => p.text);
				if (textParts.length > 0) {
					this.emit({ type: "transcript", role: "assistant", text: textParts.join("") });
				}
			}

			// Input transcription
			if (sc.inputTranscription?.text) {
				console.log(dim(`[voice] User: ${sc.inputTranscription.text}`));
				this.emit({ type: "transcript", role: "user", text: sc.inputTranscription.text });
			}
		}
	}

	private async handleToolCall(toolCall: any): Promise<void> {
		if (!this.toolHandler) return;

		const functionCalls = toolCall.functionCalls || [];
		const responses: any[] = [];

		for (const fc of functionCalls) {
			if (fc.name !== "run_agent") {
				console.error(dim(`[voice] Unknown Gemini function call: ${fc.name}`));
				responses.push({ id: fc.id, name: fc.name, response: { error: `Unknown function: ${fc.name}` } });
				continue;
			}

			const queryArg = fc.args?.query;
			if (!queryArg) {
				responses.push({ id: fc.id, name: fc.name, response: { error: "Missing query argument" } });
				continue;
			}

			console.log(dim(`[voice] Agent query: ${queryArg}`));
			this.emit({ type: "agent_working", query: queryArg });

			try {
				const result = await this.toolHandler(queryArg);
				console.log(dim(`[voice] Agent response: ${result.slice(0, 200)}${result.length > 200 ? "..." : ""}`));
				responses.push({ id: fc.id, name: fc.name, response: { result } });
				this.emit({ type: "agent_done", result: result.slice(0, 500) });
			} catch (err: any) {
				console.error(dim(`[voice] Agent error: ${err.message}`));
				responses.push({ id: fc.id, name: fc.name, response: { error: err.message } });
				this.emit({ type: "error", message: err.message });
			}
		}

		this.sendRaw({
			toolResponse: { functionResponses: responses },
		});
	}

	private sendRaw(msg: any): void {
		if (this.ws && this.ws.readyState === WebSocket.OPEN) {
			this.ws.send(JSON.stringify(msg));
		}
	}
}
