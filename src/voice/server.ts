import { createServer, type Server, type IncomingMessage } from "http";
import { WebSocketServer, WebSocket as WS } from "ws";
import { query } from "../sdk.js";
import type { VoiceServerOptions } from "./adapter.js";
import { readFileSync, existsSync } from "fs";
import { join } from "path";

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string) => `\x1b[1m${s}\x1b[0m`;

export async function startVoiceServer(opts: VoiceServerOptions): Promise<() => Promise<void>> {
	const port = opts.port || 3333;
	const apiKey = opts.adapterConfig.apiKey;
	const voiceName = opts.adapterConfig.voice || "ash";
	const realtimeModel = opts.adapterConfig.model || "gpt-4o-realtime-preview";

	// Tool handler: runs gitclaw query and collects response text
	const toolHandler = async (prompt: string): Promise<string> => {
		const result = query({
			prompt,
			dir: opts.agentDir,
			model: opts.model,
			env: opts.env,
		});

		let text = "";
		for await (const msg of result) {
			if (msg.type === "assistant" && msg.content) {
				text += msg.content;
			}
		}

		return text || "(no response)";
	};

	// Serve the test page HTML
	const serveTestPage = (res: any) => {
		res.writeHead(200, { "Content-Type": "text/html" });
		res.end(TEST_PAGE_HTML);
	};

	// HTTP server
	const httpServer: Server = createServer((req, res) => {
		// CORS
		res.setHeader("Access-Control-Allow-Origin", "*");
		res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
		res.setHeader("Access-Control-Allow-Headers", "Content-Type");

		if (req.method === "OPTIONS") {
			res.writeHead(204);
			return res.end();
		}

		if (req.url === "/health") {
			res.writeHead(200, { "Content-Type": "application/json" });
			res.end(JSON.stringify({ status: "ok" }));
		} else if (req.url === "/" || req.url === "/test") {
			serveTestPage(res);
		} else {
			res.writeHead(404);
			res.end();
		}
	});

	// WebSocket server — relay between browser and OpenAI Realtime
	const wss = new WebSocketServer({ server: httpServer });

	wss.on("connection", (browserWs: WS, req: IncomingMessage) => {
		console.log(dim("[voice] Browser connected"));

		// Connect to OpenAI Realtime
		const openaiUrl = `wss://api.openai.com/v1/realtime?model=${realtimeModel}`;
		const openaiWs = new WS(openaiUrl, {
			headers: {
				Authorization: `Bearer ${apiKey}`,
				"OpenAI-Beta": "realtime=v1",
			},
		});

		let sessionReady = false;

		openaiWs.on("open", () => {
			console.log(dim("[voice] Connected to OpenAI Realtime"));

			// Configure session
			openaiWs.send(JSON.stringify({
				type: "session.update",
				session: {
					instructions:
						"You are a voice interface for GitClaw, a powerful AI agent with access to the terminal, file system, and git. " +
						"You MUST use the run_agent tool for ANY request that involves doing something — running commands, opening apps, reading files, writing code, searching, browsing, installing packages, git operations, or anything actionable. " +
						"Only respond directly for simple greetings, clarifying questions, or when the user explicitly asks YOU a question. " +
						"When in doubt, use run_agent. Speak concisely — summarize the tool result in 1-2 sentences.",
					voice: voiceName,
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
			}));
		});

		openaiWs.on("message", async (data) => {
			const event = JSON.parse(data.toString());

			// Handle tool calls server-side
			if (event.type === "response.function_call_arguments.done") {
				const callId = event.call_id;
				if (event.name === "run_agent") {
					try {
						const args = JSON.parse(event.arguments);
						console.log(dim(`[voice] Agent query: ${args.query}`));

						// Notify browser
						safeSend(browserWs, JSON.stringify({
							type: "agent.working",
							query: args.query,
						}));

						const result = await toolHandler(args.query);
						console.log(dim(`[voice] Agent response: ${result.slice(0, 200)}${result.length > 200 ? "..." : ""}`));

						// Send function output back to OpenAI
						openaiWs.send(JSON.stringify({
							type: "conversation.item.create",
							item: {
								type: "function_call_output",
								call_id: callId,
								output: result,
							},
						}));
						openaiWs.send(JSON.stringify({ type: "response.create" }));

						safeSend(browserWs, JSON.stringify({
							type: "agent.done",
							result: result.slice(0, 500),
						}));
					} catch (err: any) {
						console.error(dim(`[voice] Agent error: ${err.message}`));
						openaiWs.send(JSON.stringify({
							type: "conversation.item.create",
							item: {
								type: "function_call_output",
								call_id: callId,
								output: `Error: ${err.message}`,
							},
						}));
						openaiWs.send(JSON.stringify({ type: "response.create" }));
					}
					return; // Don't forward tool call events to browser
				}
			}

			// Log transcriptions
			if (event.type === "conversation.item.input_audio_transcription.completed") {
				console.log(dim(`[voice] User: ${event.transcript}`));
			}

			// Forward everything else to browser (audio, transcripts, etc)
			safeSend(browserWs, data.toString());
		});

		openaiWs.on("error", (err) => {
			console.error(dim(`[voice] OpenAI error: ${err.message}`));
			safeSend(browserWs, JSON.stringify({ type: "error", message: err.message }));
		});

		openaiWs.on("close", () => {
			console.log(dim("[voice] OpenAI disconnected"));
			browserWs.close();
		});

		// Forward browser audio to OpenAI
		browserWs.on("message", (data) => {
			if (openaiWs.readyState === WS.OPEN) {
				openaiWs.send(data.toString());
			}
		});

		browserWs.on("close", () => {
			console.log(dim("[voice] Browser disconnected"));
			openaiWs.close();
		});
	});

	await new Promise<void>((resolve) => {
		httpServer.listen(port, () => resolve());
	});

	console.log(bold(`Voice server running on :${port}`));
	console.log(dim(`[voice] Open http://localhost:${port} in your browser to test`));

	return async () => {
		wss.close();
		await new Promise<void>((resolve, reject) => {
			httpServer.close((err) => (err ? reject(err) : resolve()));
		});
		console.log(dim("[voice] Server stopped"));
	};
}

function safeSend(ws: WS, data: string) {
	if (ws.readyState === WS.OPEN) {
		ws.send(data);
	}
}

const TEST_PAGE_HTML = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>GitClaw Voice</title>
<style>
  @import url('https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@300;400;500;600;700&display=swap');
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    background: #0a0a0a; color: #e4e4e4;
    font-family: 'IBM Plex Mono', monospace;
    display: flex; align-items: center; justify-content: center;
    min-height: 100vh; padding: 20px;
  }
  .container {
    max-width: 480px; width: 100%; text-align: center;
  }
  .logo { width: 64px; height: 64px; margin-bottom: 12px; }
  h1 { font-size: 24px; margin-bottom: 4px; }
  h1 span { color: #22c55e; }
  .subtitle { font-size: 11px; color: #777; margin-bottom: 32px; }
  .mic-btn {
    width: 120px; height: 120px; border-radius: 50%;
    background: #111; border: 2px solid #222;
    cursor: pointer; display: flex; align-items: center; justify-content: center;
    margin: 0 auto 24px; transition: all 0.3s;
    position: relative;
  }
  .mic-btn:hover { border-color: #22c55e40; }
  .mic-btn.active { border-color: #22c55e; box-shadow: 0 0 40px rgba(34,197,94,0.15); }
  .mic-btn.active .ring {
    position: absolute; inset: -8px; border-radius: 50%;
    border: 1px solid #22c55e30;
    animation: pulse 2s ease-in-out infinite;
  }
  .mic-btn svg { width: 32px; height: 32px; }
  .mic-btn.active svg { color: #22c55e; }
  .mic-btn:not(.active) svg { color: #555; }
  @keyframes pulse {
    0%, 100% { transform: scale(1); opacity: 0.5; }
    50% { transform: scale(1.15); opacity: 0; }
  }
  .status {
    font-size: 11px; color: #555; margin-bottom: 24px;
    min-height: 16px;
  }
  .status.connected { color: #22c55e; }
  .status.error { color: #ef4444; }
  .log {
    background: #111; border: 1px solid #1a1a1a; border-radius: 8px;
    padding: 16px; text-align: left; max-height: 300px;
    overflow-y: auto; font-size: 11px; line-height: 1.8;
  }
  .log:empty::before {
    content: "Conversation will appear here...";
    color: #333;
  }
  .log .user { color: #777; }
  .log .user::before { content: "You: "; color: #22c55e; font-weight: 600; }
  .log .assistant { color: #ccc; }
  .log .assistant::before { content: "Agent: "; color: #22c55e; font-weight: 600; }
  .log .system { color: #555; font-style: italic; }
  .log .tool { color: #f97316; }
  .log .tool::before { content: "⚡ "; }
</style>
</head>
<body>
<div class="container">
  <img src="/gitclaw-logo.png" alt="GitClaw" class="logo" onerror="this.style.display='none'">
  <h1>Git<span>Claw</span> Voice</h1>
  <p class="subtitle">speak to your agent</p>

  <button class="mic-btn" id="micBtn" onclick="toggleVoice()">
    <div class="ring" style="display:none"></div>
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
      <path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/>
      <path d="M19 10v2a7 7 0 0 1-14 0v-2"/>
      <line x1="12" x2="12" y1="19" y2="22"/>
    </svg>
  </button>

  <div class="status" id="status">Click the mic to start</div>
  <div class="log" id="log"></div>
</div>

<script>
let ws = null;
let audioCtx = null;
let mediaStream = null;
let processor = null;
let active = false;

const micBtn = document.getElementById('micBtn');
const statusEl = document.getElementById('status');
const logEl = document.getElementById('log');

// Audio playback queue
let audioQueue = [];
let isPlaying = false;
let currentSource = null;

function setStatus(text, cls) {
  statusEl.textContent = text;
  statusEl.className = 'status ' + (cls || '');
}

function appendLog(text, cls) {
  const div = document.createElement('div');
  div.className = cls;
  div.textContent = text;
  logEl.appendChild(div);
  logEl.scrollTop = logEl.scrollHeight;
}

async function toggleVoice() {
  if (active) {
    stopVoice();
  } else {
    await startVoice();
  }
}

async function startVoice() {
  try {
    setStatus('Connecting...', '');

    // Get microphone
    mediaStream = await navigator.mediaDevices.getUserMedia({ audio: true });

    // Connect WebSocket to gitclaw voice server
    const wsUrl = 'ws://' + window.location.host;
    ws = new WebSocket(wsUrl);

    ws.onopen = () => {
      setStatus('Connected — speak now', 'connected');
      active = true;
      micBtn.classList.add('active');
      micBtn.querySelector('.ring').style.display = '';
      startAudioCapture();
    };

    ws.onmessage = (e) => {
      const event = JSON.parse(e.data);
      handleServerEvent(event);
    };

    ws.onerror = () => {
      setStatus('Connection error', 'error');
    };

    ws.onclose = () => {
      if (active) {
        setStatus('Disconnected', 'error');
        stopVoice();
      }
    };
  } catch (err) {
    setStatus('Mic access denied', 'error');
    console.error(err);
  }
}

function stopVoice() {
  active = false;
  micBtn.classList.remove('active');
  micBtn.querySelector('.ring').style.display = 'none';

  if (processor) { processor.disconnect(); processor = null; }
  if (mediaStream) { mediaStream.getTracks().forEach(t => t.stop()); mediaStream = null; }
  if (ws) { ws.close(); ws = null; }
  if (currentSource) { currentSource.stop(); currentSource = null; }
  audioQueue = [];
  isPlaying = false;

  setStatus('Click the mic to start', '');
}

function startAudioCapture() {
  audioCtx = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 24000 });
  const source = audioCtx.createMediaStreamSource(mediaStream);

  // Use ScriptProcessor to get raw PCM
  processor = audioCtx.createScriptProcessor(4096, 1, 1);
  source.connect(processor);
  processor.connect(audioCtx.destination);

  processor.onaudioprocess = (e) => {
    if (!active || !ws || ws.readyState !== WebSocket.OPEN) return;

    const input = e.inputBuffer.getChannelData(0);
    // Convert float32 to int16
    const int16 = new Int16Array(input.length);
    for (let i = 0; i < input.length; i++) {
      int16[i] = Math.max(-32768, Math.min(32767, Math.floor(input[i] * 32768)));
    }

    // Send as base64 encoded PCM
    const base64 = btoa(String.fromCharCode(...new Uint8Array(int16.buffer)));
    ws.send(JSON.stringify({
      type: 'input_audio_buffer.append',
      audio: base64,
    }));
  };
}

let responseText = '';

function handleServerEvent(event) {
  switch (event.type) {
    case 'session.created':
    case 'session.updated':
      break;

    case 'conversation.item.input_audio_transcription.completed':
      if (event.transcript) appendLog(event.transcript.trim(), 'user');
      break;

    case 'response.audio.delta':
      if (event.delta) {
        playAudioDelta(event.delta);
      }
      break;

    case 'response.audio_transcript.delta':
      responseText += (event.delta || '');
      break;

    case 'response.audio_transcript.done':
      if (responseText.trim()) appendLog(responseText.trim(), 'assistant');
      responseText = '';
      break;

    case 'agent.working':
      appendLog('Running: ' + event.query, 'tool');
      setStatus('Agent working...', 'connected');
      break;

    case 'agent.done':
      setStatus('Connected — speak now', 'connected');
      break;

    case 'error':
      appendLog('Error: ' + (event.error?.message || event.message || 'unknown'), 'system');
      break;
  }
}

function playAudioDelta(base64) {
  if (!audioCtx) audioCtx = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 24000 });

  // Decode base64 to Int16 PCM
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  const int16 = new Int16Array(bytes.buffer);

  // Convert to Float32
  const float32 = new Float32Array(int16.length);
  for (let i = 0; i < int16.length; i++) float32[i] = int16[i] / 32768;

  const buffer = audioCtx.createBuffer(1, float32.length, 24000);
  buffer.getChannelData(0).set(float32);

  audioQueue.push(buffer);
  if (!isPlaying) playNext();
}

let nextPlayTime = 0;

function playNext() {
  if (audioQueue.length === 0) {
    isPlaying = false;
    return;
  }
  isPlaying = true;
  const buffer = audioQueue.shift();
  const source = audioCtx.createBufferSource();
  source.buffer = buffer;
  source.connect(audioCtx.destination);

  const now = audioCtx.currentTime;
  const startTime = Math.max(now, nextPlayTime);
  source.start(startTime);
  nextPlayTime = startTime + buffer.duration;

  source.onended = () => {
    if (audioQueue.length > 0) playNext();
    else isPlaying = false;
  };
  currentSource = source;
}
</script>
</body>
</html>`;
