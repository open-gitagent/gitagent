import { createServer, type Server, type IncomingMessage, type ServerResponse } from "http";
import { WebSocketServer, WebSocket as WS } from "ws";
import { query } from "../sdk.js";
import type { VoiceServerOptions, ClientMessage, ServerMessage, MultimodalAdapter } from "./adapter.js";
import { readFileSync, readdirSync, statSync, existsSync, writeFileSync, mkdirSync, appendFileSync, rmSync } from "fs";
import { execSync } from "child_process";
import { join, dirname, resolve, relative } from "path";
import { writeFile, readFile, mkdir, stat } from "fs/promises";
import { fileURLToPath } from "url";
import { OpenAIRealtimeAdapter } from "./openai-realtime.js";
import { GeminiLiveAdapter } from "./gemini-live.js";
import { ComposioAdapter } from "../composio/index.js";
import type { GCToolDefinition } from "../sdk-types.js";
import { appendMessage, loadHistory, deleteHistory, summarizeHistory } from "./chat-history.js";
import { getVoiceContext, getAgentContext } from "../context.js";
import { discoverSkills } from "../skills.js";
import { discoverWorkflows, loadFlowDefinition, saveFlowDefinition, deleteFlowDefinition } from "../workflows.js";
import { discoverSchedules, saveSchedule, deleteSchedule, updateScheduleMeta } from "../schedules.js";
import { startScheduler, stopScheduler, reloadSchedules, executeScheduledJob } from "../schedule-runner.js";
import cron from "node-cron";

const dim = (s: string) => `\x1b[2m${s}\x1b[0m`;
const bold = (s: string) => `\x1b[1m${s}\x1b[0m`;

// ── Background memory saver ────────────────────────────────────────────
// Patterns that indicate the user is sharing personal info worth saving.
// This runs server-side so we don't depend on the voice LLM deciding to save.
const MEMORY_PATTERNS = [
	/\bi (?:like|love|enjoy|prefer|hate|dislike)\b/i,
	/\bmy (?:name|dog|cat|favorite|fav|hobby|job|car|team)\b/i,
	/\bi(?:'m| am) (?:a |into |from |working on )/i,
	/\bi(?:'m| am) \w+$/i,                           // "I am Shreyas", "I'm Zeus"
	/\bmy name is\b/i,                                // "my name is ..."
	/\bcall me\b/i,
	/\bremember (?:that|this)\b/i,
	/\bi (?:play|watch|drive|use|work with|listen to)\b/i,
	/\bi(?:'m| am) \d+/i,                             // "I'm 25", age
	/\bi (?:live|grew up|was born) (?:in|at|near)\b/i, // location info
	/\bpeople call me\b/i,
];

function isMemoryWorthy(text: string): boolean {
	return MEMORY_PATTERNS.some((p) => p.test(text));
}

// ── Moment detection for photo capture ─────────────────────────────────
const MOMENT_PATTERNS = [
	/\bhaha\b/i,
	/\blol\b/i,
	/\blmao\b/i,
	/\blove it\b/i,
	/\bthat'?s amazing\b/i,
	/\bso happy\b/i,
	/\bbest day\b/i,
	/\bwe did it\b/i,
	/\bnailed it\b/i,
	/\blet'?s go\b/i,
	/\bhell yeah\b/i,
	/\bawesome\b/i,
	/\bthank you so much\b/i,
	/\bfirst time\b/i,
	/\bmilestone\b/i,
	/\bcelebrat/i,
	/\bincredible\b/i,
];

function isMomentWorthy(text: string): boolean {
	return MOMENT_PATTERNS.some((p) => p.test(text));
}

let vitalsTokenCount = 0;

// ── Centralized vitals snapshot ────────────────────────────────────────
// All surfaces (API, UI, logs) share the same cached snapshot so values
// are always consistent regardless of who reads them or when.
interface VitalsSnapshot {
	cpu: number;
	mem: number;
	heapUsed: number;
	heapTotal: number;
	uptime: number;
	tokens: number;
	ts: number; // unix-ms when this snapshot was taken
}

let _lastCpuUsage = process.cpuUsage();
let _lastCpuTime = process.hrtime.bigint();
let _vitalsCache: VitalsSnapshot | null = null;
const VITALS_CACHE_MS = 1000; // cache for 1s — all readers within 1s see identical values

function getVitalsSnapshot(): VitalsSnapshot {
	const now = Date.now();
	if (_vitalsCache && now - _vitalsCache.ts < VITALS_CACHE_MS) return _vitalsCache;

	const mem = process.memoryUsage();
	const currentCpu = process.cpuUsage();
	const currentTime = process.hrtime.bigint();

	// Delta-based CPU: measure CPU microseconds consumed since last sample
	const userDelta = currentCpu.user - _lastCpuUsage.user;
	const sysDelta = currentCpu.system - _lastCpuUsage.system;
	const wallDeltaUs = Number(currentTime - _lastCpuTime) / 1000; // ns → µs
	const cpuPercent = wallDeltaUs > 0
		? Math.min(100, Math.round((userDelta + sysDelta) / wallDeltaUs * 100))
		: 0;

	_lastCpuUsage = currentCpu;
	_lastCpuTime = currentTime;

	_vitalsCache = {
		cpu: cpuPercent,
		mem: Math.round(mem.rss / 1024 / 1024),
		heapUsed: Math.round(mem.heapUsed / 1024 / 1024),
		heapTotal: Math.round(mem.heapTotal / 1024 / 1024),
		uptime: Math.round(process.uptime()),
		tokens: vitalsTokenCount,
		ts: now,
	};
	return _vitalsCache;
}
const PHOTOS_DIR = "memory/photos";
const INDEX_FILE = "memory/photos/INDEX.md";
const LATEST_FRAME_FILE = "memory/.latest-frame.jpg";
const LATEST_SCREEN_FILE = "memory/.latest-screen.jpg";

function slugify(text: string): string {
	return text
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, "-")
		.replace(/^-|-$/g, "")
		.slice(0, 40);
}

// ── Mood tracking ──────────────────────────────────────────────────────
type Mood = "happy" | "frustrated" | "curious" | "excited" | "calm";
const MOOD_SIGNALS: { mood: Mood; patterns: RegExp[] }[] = [
	{ mood: "happy", patterns: [/\bhaha\b/i, /\blol\b/i, /\blove it\b/i, /\bthat'?s great\b/i, /\bnice\b/i, /\bawesome\b/i, /\bamazing\b/i] },
	{ mood: "frustrated", patterns: [/\bugh\b/i, /\bwhat the\b/i, /\bdamn\b/i, /\bstill broken\b/i, /\bnot working\b/i, /\bwhy (?:is|does|won'?t)\b/i, /\bfuck\b/i] },
	{ mood: "curious", patterns: [/\bhow (?:do|does|can|would)\b/i, /\bwhat (?:is|are|if)\b/i, /\bwhy (?:do|does|is)\b/i, /\bexplain\b/i, /\btell me about\b/i] },
	{ mood: "excited", patterns: [/\blet'?s go\b/i, /\bhell yeah\b/i, /\bwe did it\b/i, /\bnailed it\b/i, /\byes!\b/i, /\bfinally\b/i] },
	{ mood: "calm", patterns: [/\bokay\b/i, /\bsure\b/i, /\bcool\b/i, /\bsounds good\b/i, /\bgot it\b/i] },
];

function detectMood(text: string): Mood | null {
	for (const { mood, patterns } of MOOD_SIGNALS) {
		if (patterns.some((p) => p.test(text))) return mood;
	}
	return null;
}

interface MoodCounts { happy: number; frustrated: number; curious: number; excited: number; calm: number }

function dominantMood(counts: MoodCounts): Mood {
	let best: Mood = "calm";
	let max = 0;
	for (const [mood, count] of Object.entries(counts) as [Mood, number][]) {
		if (count > max) { max = count; best = mood; }
	}
	return best;
}

async function saveMoodEntry(agentDir: string, counts: MoodCounts, messageCount: number): Promise<void> {
	if (messageCount < 3) return; // Skip trivially short sessions

	const now = new Date();
	const pad = (n: number) => String(n).padStart(2, "0");
	const date = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
	const time = `${pad(now.getHours())}:${pad(now.getMinutes())}`;
	const mood = dominantMood(counts);

	const moodPath = join(agentDir, "memory", "mood.md");
	let existing = "";
	try { existing = await readFile(moodPath, "utf-8"); } catch {
		existing = "# Mood Log\n\n";
	}

	const detail = Object.entries(counts).filter(([, v]) => v > 0).map(([k, v]) => `${k}:${v}`).join(" ");
	existing += `- ${date} ${time} — **${mood}** (${detail}) [${messageCount} msgs]\n`;

	await mkdir(join(agentDir, "memory"), { recursive: true });
	await writeFile(moodPath, existing, "utf-8");

	try {
		execSync(`git add "memory/mood.md" && git commit -m "Mood: ${mood} session (${date} ${time})"`, {
			cwd: agentDir, stdio: "pipe",
		});
	} catch { /* file saved even if commit fails */ }
}

// ── Session journaling ─────────────────────────────────────────────────
async function writeJournalEntry(
	agentDir: string,
	branch: string,
	moodCounts: MoodCounts,
	model?: string,
	env?: string,
): Promise<void> {
	const messages = loadHistory(agentDir, branch);
	if (messages.length < 5) return;

	const lines: string[] = [];
	for (const msg of messages.slice(-50)) {
		if (msg.type === "transcript") lines.push(`${msg.role}: ${msg.text}`);
		else if (msg.type === "agent_done") lines.push(`agent: ${msg.result.slice(0, 200)}`);
	}
	if (lines.length < 3) return;

	let transcript = lines.join("\n");
	if (transcript.length > 3000) transcript = transcript.slice(-3000);

	const mood = dominantMood(moodCounts);
	const prompt = `Write a brief journal entry (3-5 sentences) reflecting on this conversation session. Mood was mostly: ${mood}. Note what was accomplished, any unfinished threads, and how the user seemed. Write in first person as the agent. Be genuine, not corporate.\n\nTranscript:\n${transcript}`;

	try {
		const result = query({
			prompt,
			dir: agentDir,
			model,
			env,
			maxTurns: 1,
			replaceBuiltinTools: true,
			tools: [],
			systemPrompt: "You are journaling about your day as an AI assistant. Write naturally and briefly.",
		});

		let entry = "";
		for await (const msg of result) {
			if (msg.type === "assistant" && msg.content) entry += msg.content;
		}
		entry = entry.trim();
		if (!entry) return;

		const now = new Date();
		const pad = (n: number) => String(n).padStart(2, "0");
		const date = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
		const time = `${pad(now.getHours())}:${pad(now.getMinutes())}`;

		const journalDir = join(agentDir, "memory", "journal");
		await mkdir(journalDir, { recursive: true });
		const journalPath = join(journalDir, `${date}.md`);

		let existing = "";
		try { existing = await readFile(journalPath, "utf-8"); } catch {
			existing = `# Journal — ${date}\n\n`;
		}
		existing += `### ${time} (${mood})\n${entry}\n\n`;
		await writeFile(journalPath, existing, "utf-8");

		try {
			execSync(`git add "memory/journal/${date}.md" && git commit -m "Journal: ${date} ${time} session reflection"`, {
				cwd: agentDir, stdio: "pipe",
			});
			console.error(dim(`[voice] Journal entry written for ${date} ${time}`));
		} catch { /* saved even if commit fails */ }
	} catch (err: any) {
		console.error(dim(`[voice] Journal write failed: ${err.message}`));
	}
}

async function capturePhoto(
	agentDir: string,
	reason: string,
	frameData?: Buffer,
): Promise<void> {
	// If no frame passed directly, read from temp file
	let frame = frameData;
	if (!frame) {
		const framePath = join(agentDir, LATEST_FRAME_FILE);
		try {
			const frameStat = await stat(framePath);
			if (Date.now() - frameStat.mtimeMs > 5000) {
				console.error(dim("[voice] No recent camera frame, skipping photo capture"));
				return;
			}
			frame = await readFile(framePath);
		} catch {
			console.error(dim("[voice] No camera frame available, skipping photo capture"));
			return;
		}
	}

	const now = new Date();
	const pad = (n: number) => String(n).padStart(2, "0");
	const datePart = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
	const timePart = `${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
	const slug = slugify(reason);
	const filename = `${datePart}_${timePart}_${slug}.jpg`;
	const photoRelPath = `${PHOTOS_DIR}/${filename}`;
	const photoAbsPath = join(agentDir, photoRelPath);

	await mkdir(join(agentDir, PHOTOS_DIR), { recursive: true });
	await writeFile(photoAbsPath, frame);

	// Update INDEX.md
	const indexPath = join(agentDir, INDEX_FILE);
	let indexContent = "";
	try {
		indexContent = await readFile(indexPath, "utf-8");
	} catch {
		indexContent = "# Memorable Moments\n\nPhotos captured during happy and memorable moments.\n\n";
	}
	const entry = `- **${datePart} ${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}** — ${reason} → [\`${filename}\`](${filename})\n`;
	indexContent += entry;
	await writeFile(indexPath, indexContent, "utf-8");

	// Git add + commit
	const commitMsg = `Capture moment: ${reason}`;
	try {
		execSync(`git add "${photoRelPath}" "${INDEX_FILE}" && git commit -m "${commitMsg.replace(/"/g, '\\"')}"`, {
			cwd: agentDir,
			stdio: "pipe",
		});
		console.error(dim(`[voice] Photo captured: ${filename}`));
	} catch (err: any) {
		console.error(dim(`[voice] Photo saved but git commit failed: ${err.stderr?.toString().trim() || "unknown"}`));
	}
}

function saveMemoryInBackground(
	text: string,
	agentDir: string,
	model?: string,
	env?: string,
	onStart?: () => void,
	onComplete?: () => void,
): void {
	const prompt = `The user just said: "${text}"\n\nSave any personal information, preferences, or facts about the user to memory. Use the memory tool to write or update a memory file. Use a descriptive commit message like "Remember: user likes mustangs" or "Save preference: favorite game is GTA 5". Be concise. If there's nothing meaningful to save, do nothing.`;
	console.error(dim(`[voice] Background memory save triggered for: "${text.slice(0, 60)}..."`));

	if (onStart) onStart();

	// Fire and forget — don't block the voice conversation
	(async () => {
		try {
			const result = query({
				prompt,
				dir: agentDir,
				model,
				env,
				maxTurns: 3,
			});
			// Drain the iterator to completion
			for await (const msg of result) {
				if (msg.type === "tool_use") {
					console.error(dim(`[voice/memory] Tool: ${msg.toolName}`));
				}
			}
			console.error(dim("[voice/memory] Background save complete"));
			if (onComplete) onComplete();
		} catch (err: any) {
			console.error(dim(`[voice/memory] Background save failed: ${err.message}`));
			if (onComplete) onComplete();
		}
	})();
}

/** Load .env file into process.env (won't overwrite existing vars) */
function loadEnvFile(dir: string) {
	const envPath = join(dir, ".env");
	try {
		const content = readFileSync(envPath, "utf-8");
		for (const line of content.split("\n")) {
			const trimmed = line.trim();
			if (!trimmed || trimmed.startsWith("#")) continue;
			const eq = trimmed.indexOf("=");
			if (eq < 1) continue;
			const key = trimmed.slice(0, eq).trim();
			let val = trimmed.slice(eq + 1).trim();
			// Strip surrounding quotes
			if ((val.startsWith('"') && val.endsWith('"')) || (val.startsWith("'") && val.endsWith("'"))) {
				val = val.slice(1, -1);
			}
			if (!process.env[key]) {
				process.env[key] = val;
			}
		}
	} catch {
		// No .env file — that's fine
	}
}

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
	// Load .env from agent directory (won't overwrite existing env vars)
	loadEnvFile(resolve(opts.agentDir));

	const port = opts.port || 3333;
	let agentName = "GitClaw";
	try {
		const yamlRaw = readFileSync(join(resolve(opts.agentDir), "agent.yaml"), "utf-8");
		const m = yamlRaw.match(/^name:\s*(.+)$/m);
		if (m) agentName = m[1].trim();
	} catch { /* fallback to default */ }
	const uiHtml = loadUIHtml()
		.replace(/\{\{AGENT_NAME\}\}/g, agentName)
		.replace(/\{\{HAS_COMPOSIO\}\}/g, process.env.COMPOSIO_API_KEY ? "true" : "false");

	// Current date/time context injected into every query
	function getCurrentDateTimeContext(): string {
		const now = new Date();
		const day = now.toLocaleDateString("en-US", { weekday: "long" });
		const date = now.toLocaleDateString("en-US", { year: "numeric", month: "long", day: "numeric" });
		const time = now.toLocaleTimeString("en-US", { hour: "2-digit", minute: "2-digit", hour12: true });
		return `Current date and time: ${day}, ${date}, ${time}.`;
	}

	// Shared helper: fetch Composio tools + build prompt suffix for any channel
	async function getComposioContext(prompt: string): Promise<{ tools: GCToolDefinition[]; promptSuffix: string | undefined }> {
		let composioTools: GCToolDefinition[] = [];
		let connectedSlugs: string[] = [];
		if (composioAdapter) {
			try {
				connectedSlugs = await composioAdapter.getConnectedToolkitSlugs();
				console.error(`[voice] Connected toolkit slugs: [${connectedSlugs.join(", ")}]`);
				if (connectedSlugs.length > 0) {
					composioTools = await composioAdapter.getToolsForQuery(prompt);
					console.error(`[voice] Semantic search returned ${composioTools.length} tools`);
					if (composioTools.length === 0) {
						const allTools = await composioAdapter.getTools();
						composioTools = allTools.slice(0, 15);
						console.error(`[voice] Fallback capped to ${composioTools.length}/${allTools.length} tools`);
					}
					console.error(`[voice] Composio: ${composioTools.length} tools: ${composioTools.map(t => t.name).join(", ")}`);
				} else {
					console.error(`[voice] No connected toolkits found for user`);
				}
			} catch (err: any) {
				console.error(`[voice] Composio tool fetch FAILED: ${err.message}\n${err.stack}`);
			}
		} else {
			console.error(`[voice] composioAdapter is NULL — COMPOSIO_API_KEY not set?`);
		}

		let promptSuffix: string | undefined;
		if (composioAdapter) {
			const parts = [
				`You have access to external services via Composio integration (Gmail, Google Calendar, GitHub, Slack, and many more).`,
				`You CAN perform real actions — send emails, read emails, check calendars, create events, manage repos, etc.`,
				`NEVER tell the user you "can't access" or "don't have access to" external services. Always attempt to use the available Composio tools (prefixed "composio_") first.`,
				`When the user asks to send an email, use the composio SEND_EMAIL tool directly — do NOT create a draft unless they explicitly ask for a draft.`,
				`When the user asks about their calendar, use the composio calendar tools to fetch real events.`,
				`Prefer Composio tools over CLI commands for any external service interaction.`,
			];
			if (connectedSlugs.length > 0) {
				const services = connectedSlugs.map((s) => s.replace(/_/g, " ")).join(", ");
				parts.unshift(`Currently connected services: ${services}.`);
			}
			promptSuffix = parts.join(" ");
		}

		return { tools: composioTools, promptSuffix };
	}

	// Creates a per-connection tool handler that can stream events to the browser
	function createToolHandler(sendToBrowser: (msg: ServerMessage) => void) {
		return async (prompt: string): Promise<string> => {
			const { tools: composioTools, promptSuffix: composioPromptSuffix } = await getComposioContext(prompt);

			let systemPromptSuffix = getCurrentDateTimeContext();
			systemPromptSuffix += "\nWhen creating files (PDFs, images, documents, markdown files, code output, etc.), write them to the workspace/ directory by default. If the user explicitly specifies a different path, use the path they requested.";
			if (whatsappSock && whatsappConnected) {
				systemPromptSuffix += "\nYou can send WhatsApp messages using the send_whatsapp_message tool and set up auto-response triggers using create_trigger.";
			} else {
				systemPromptSuffix += "\nYou can set up auto-response triggers using create_trigger for when messaging platforms are connected.";
			}
			if (composioPromptSuffix) systemPromptSuffix += "\n\n" + composioPromptSuffix;

			// Inject shared context (memory + conversation summary)
			const agentContext = await getAgentContext(opts.agentDir, activeBranch);
			if (agentContext) {
				systemPromptSuffix = (systemPromptSuffix || "") + "\n\n" + agentContext;
			}

			const uiTools: GCToolDefinition[] = [
				...createTriggerTools(opts.agentDir),
				...(whatsappSock && whatsappConnected ? createWhatsAppTools(whatsappSock, opts.agentDir) : []),
				...composioTools,
			];
			const result = query({
				prompt,
				dir: opts.agentDir,
				model: opts.model,
				env: opts.env,
				...(uiTools.length ? { tools: uiTools } : {}),
				...(systemPromptSuffix ? { systemPromptSuffix } : {}),
			});

			let text = "";
			const toolResults: string[] = [];
			const errors: string[] = [];

			for await (const msg of result) {
				if (msg.type === "assistant" && msg.content) {
					text += msg.content;
					vitalsTokenCount += Math.ceil(msg.content.length / 4);
				} else if (msg.type === "tool_use") {
					sendToBrowser({ type: "tool_call", toolName: msg.toolName, args: msg.args });
					console.log(dim(`[voice] Tool call: ${msg.toolName}(${JSON.stringify(msg.args).slice(0, 80)})`));
				} else if (msg.type === "tool_result") {
					sendToBrowser({ type: "tool_result", toolName: msg.toolName, content: msg.content, isError: msg.isError });
					if (msg.content) { toolResults.push(msg.content); vitalsTokenCount += Math.ceil(msg.content.length / 4); }
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

	// ── SkillFlow execution ─────────────────────────────────────────────
	// ── Approval gate state ────────────────────────────────────────────
	let pendingApproval: { resolve: (approved: boolean) => void } | null = null;

	function handleApprovalReply(text: string): boolean {
		if (!pendingApproval) return false;
		const lower = text.trim().toLowerCase();
		if (["yes", "approve", "continue", "ok", "go", "y", "proceed"].includes(lower)) {
			pendingApproval.resolve(true);
			pendingApproval = null;
			return true;
		}
		if (["no", "deny", "stop", "cancel", "abort", "n", "reject"].includes(lower)) {
			pendingApproval.resolve(false);
			pendingApproval = null;
			return true;
		}
		return false;
	}

	async function sendApprovalRequest(channel: string, message: string): Promise<boolean> {
		// Send message via the chosen channel
		if (channel === "telegram" && telegramToken && lastTelegramChatId) {
			await fetch(`https://api.telegram.org/bot${telegramToken}/sendMessage`, {
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ chat_id: lastTelegramChatId, text: message }),
			});
		} else if (channel === "whatsapp" && whatsappSock && whatsappConnected && lastWhatsAppJid) {
			const sent = await whatsappSock.sendMessage(lastWhatsAppJid, { text: message });
			if (sent?.key?.id) whatsappSentIds.add(sent.key.id);
		} else {
			return true; // No channel available — auto-approve
		}

		// Wait for reply (timeout after 5 minutes)
		return new Promise<boolean>((resolve) => {
			pendingApproval = { resolve };
			const timeout = setTimeout(() => {
				if (pendingApproval?.resolve === resolve) {
					pendingApproval = null;
					resolve(false); // Timeout = deny
				}
			}, 5 * 60 * 1000);
			const origResolve = resolve;
			pendingApproval.resolve = (val: boolean) => {
				clearTimeout(timeout);
				origResolve(val);
			};
		});
	}

	async function executeFlow(flowName: string, userContext: string, sendToBrowser: (msg: ServerMessage) => void) {
		const flowPath = join(resolve(opts.agentDir), "workflows", flowName + ".yaml");
		const flow = await loadFlowDefinition(flowPath);

		sendToBrowser({ type: "transcript", role: "assistant",
			text: `Running flow: ${flow.name} (${flow.steps.length} steps)` });

		let runningContext = userContext;

		for (let i = 0; i < flow.steps.length; i++) {
			const step = flow.steps[i];

			// ── Approval gate step ──
			if (step.skill === "__approval_gate__") {
				const channel = step.channel || "telegram";
				const customMsg = step.prompt || "";
				const approvalMsg = customMsg
					? `⏸ Approval Required: ${customMsg}\n\nReply YES to continue or NO to cancel.`
					: `⏸ Flow "${flow.name}" paused at step ${i + 1}/${flow.steps.length}.\n\nCompleted so far:\n${runningContext.slice(0, 500)}\n\nReply YES to continue or NO to cancel.`;

				sendToBrowser({ type: "transcript", role: "assistant",
					text: `⏸ Waiting for approval via ${channel}...` });

				const approved = await sendApprovalRequest(channel, approvalMsg);

				if (!approved) {
					sendToBrowser({ type: "transcript", role: "assistant",
						text: `Flow "${flow.name}" was denied at approval gate (step ${i + 1}).` });
					return;
				}
				sendToBrowser({ type: "transcript", role: "assistant",
					text: `✓ Approval received — continuing flow.` });
				runningContext += `\n\n[Step ${i + 1}: approval gate]: Approved via ${channel}`;
				continue;
			}

			sendToBrowser({ type: "agent_working" as any, query: `Step ${i + 1}/${flow.steps.length}: ${step.skill}` } as any);

			const prompt = `Use the skill "${step.skill}" (load it with /skill:${step.skill}).
${step.prompt.replace(/\{input\}/g, userContext)}

Context from previous steps:
${runningContext}`;

			const result = query({
				prompt,
				dir: opts.agentDir,
				model: opts.model,
				env: opts.env,
			});

			let stepOutput = "";
			for await (const msg of result) {
				if (msg.type === "assistant" && msg.content) stepOutput += msg.content;
				if (msg.type === "tool_use") sendToBrowser({ type: "tool_call", toolName: msg.toolName, args: msg.args } as any);
				if (msg.type === "tool_result") sendToBrowser({ type: "tool_result", toolName: msg.toolName, content: msg.content, isError: msg.isError } as any);
			}

			runningContext += `\n\n[Step ${i + 1} result (${step.skill})]: ${stepOutput}`;
			sendToBrowser({ type: "agent_done" as any, result: `Step ${i + 1} complete` } as any);
		}

		sendToBrowser({ type: "transcript", role: "assistant", text: `Flow "${flow.name}" completed.` });
	}

	// ── File API helpers ────────────────────────────────────────────────
	const HIDDEN_DIRS = new Set([".git", "node_modules", ".gitagent", "dist", ".next", "__pycache__", ".venv"]);
	const agentRoot = resolve(opts.agentDir);
	let activeBranch = execSync("git rev-parse --abbrev-ref HEAD", { cwd: agentRoot, encoding: "utf-8" }).trim();
	const pendingShutdownWork: Promise<any>[] = [];

	// ── Composio integration (optional) ────────────────────────────────
	let composioAdapter: ComposioAdapter | null = null;
	if (process.env.COMPOSIO_API_KEY) {
		composioAdapter = new ComposioAdapter({
			apiKey: process.env.COMPOSIO_API_KEY,
			userId: process.env.COMPOSIO_USER_ID || "default",
		});
		console.log(dim("[voice] Composio integration enabled"));
	}

	// ── Telegram bot state ──────────────────────────────────────────────
	let telegramToken = process.env.TELEGRAM_BOT_TOKEN || "";
	let telegramBotInfo: any = null;
	let telegramPolling = false;
	let telegramPollTimer: ReturnType<typeof setTimeout> | null = null;
	let telegramOffset = 0;
	// Allowed Telegram usernames — comma-separated in .env, empty = allow all
	let telegramAllowedUsers = new Set(
		(process.env.TELEGRAM_ALLOWED_USERS || "")
			.split(",")
			.map(s => s.trim().toLowerCase().replace(/^@/, ""))
			.filter(Boolean),
	);

	let lastTelegramChatId: number | null = null;

	function stopTelegramPolling() {
		telegramPolling = false;
		if (telegramPollTimer) { clearTimeout(telegramPollTimer); telegramPollTimer = null; }
	}

	/** Broadcast a message to all connected browser WebSocket clients */
	function broadcastToBrowsers(msg: ServerMessage) {
		const payload = JSON.stringify(msg);
		for (const client of wss.clients) {
			if (client.readyState === 1) client.send(payload);
		}
	}

	// ── Scheduler setup ────────────────────────────────────────────────
	const scheduleSendToBrowser = (msg: ServerMessage) => {
		broadcastToBrowsers(msg);
		appendMessage(opts.agentDir, activeBranch, msg);
	};
	const headlessHandler = createToolHandler(scheduleSendToBrowser);
	const schedulerOpts = {
		agentDir: agentRoot,
		model: opts.model,
		env: opts.env,
		runPrompt: headlessHandler,
		broadcastToBrowsers,
		appendToHistory: (msg: any) => appendMessage(opts.agentDir, activeBranch, msg),
	};

	async function downloadTelegramFile(fileId: string, agentDir: string): Promise<{ path: string; name: string } | null> {
		try {
			const fRes = await fetch(`https://api.telegram.org/bot${telegramToken}/getFile?file_id=${fileId}`);
			const fData = await fRes.json() as any;
			if (!fData.ok) return null;
			const filePath = fData.result.file_path as string;
			const ext = filePath.split(".").pop() || "jpg";
			const name = `telegram_${Date.now()}.${ext}`;
			const dlUrl = `https://api.telegram.org/file/bot${telegramToken}/${filePath}`;
			const dlRes = await fetch(dlUrl);
			const buffer = Buffer.from(await dlRes.arrayBuffer());
			const wsDir = join(agentDir, "workspace");
			mkdirSync(wsDir, { recursive: true });
			const savePath = join(wsDir, name);
			writeFileSync(savePath, buffer);
			return { path: `workspace/${name}`, name };
		} catch {
			return null;
		}
	}

	/** Collect all files recursively under a dir with their mtimes */
	function snapshotFiles(dir: string, base: string = ""): Map<string, number> {
		const result = new Map<string, number>();
		try {
			for (const name of readdirSync(dir)) {
				if (name.startsWith(".") || name === "node_modules" || name === "dist") continue;
				const full = join(dir, name);
				const rel = base ? `${base}/${name}` : name;
				try {
					const st = statSync(full);
					if (st.isDirectory()) {
						for (const [k, v] of snapshotFiles(full, rel)) result.set(k, v);
					} else if (st.isFile()) {
						result.set(rel, st.mtimeMs);
					}
				} catch { /* skip */ }
			}
		} catch { /* skip */ }
		return result;
	}

	/** Find new or modified files by comparing snapshots */
	function diffSnapshots(before: Map<string, number>, after: Map<string, number>): string[] {
		const changed: string[] = [];
		for (const [path, mtime] of after) {
			if (!before.has(path) || before.get(path)! < mtime) changed.push(path);
		}
		return changed;
	}

	const SENDABLE_EXTS = new Set([
		"pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "csv", "txt", "rtf",
		"png", "jpg", "jpeg", "gif", "webp", "svg", "bmp",
		"zip", "tar", "gz", "json", "xml", "html", "css", "js", "ts", "py", "md",
		"mp3", "mp4", "wav", "ogg", "webm",
	]);

	async function sendTelegramFile(chatId: number, filePath: string, agentDir: string, caption?: string) {
		const abs = join(agentDir, filePath);
		if (!existsSync(abs)) return;
		const st = statSync(abs);
		if (st.size > 50 * 1024 * 1024) return; // Telegram 50MB limit
		const ext = filePath.split(".").pop()?.toLowerCase() || "";
		const isImage = /^(png|jpg|jpeg|gif|webp|bmp)$/.test(ext);

		const formBoundary = `----FormBoundary${Date.now()}`;
		const fileData = readFileSync(abs);
		const fileName = filePath.split("/").pop() || "file";

		// Build multipart form
		const fieldName = isImage ? "photo" : "document";
		const endpoint = isImage ? "sendPhoto" : "sendDocument";
		const parts: Buffer[] = [];
		const nl = Buffer.from("\r\n");

		// chat_id field
		parts.push(Buffer.from(`--${formBoundary}\r\nContent-Disposition: form-data; name="chat_id"\r\n\r\n${chatId}`));
		parts.push(nl);

		// caption field
		if (caption) {
			const cap = caption.length > 1024 ? caption.slice(0, 1021) + "..." : caption;
			parts.push(Buffer.from(`--${formBoundary}\r\nContent-Disposition: form-data; name="caption"\r\n\r\n${cap}`));
			parts.push(nl);
		}

		// file field
		const mimeMap: Record<string, string> = {
			pdf: "application/pdf", doc: "application/msword", docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
			xls: "application/vnd.ms-excel", xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
			ppt: "application/vnd.ms-powerpoint", pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
			png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp",
			zip: "application/zip", csv: "text/csv", txt: "text/plain", json: "application/json", md: "text/markdown",
		};
		const mime = mimeMap[ext] || "application/octet-stream";
		parts.push(Buffer.from(`--${formBoundary}\r\nContent-Disposition: form-data; name="${fieldName}"; filename="${fileName}"\r\nContent-Type: ${mime}\r\n\r\n`));
		parts.push(fileData);
		parts.push(nl);
		parts.push(Buffer.from(`--${formBoundary}--\r\n`));

		const body = Buffer.concat(parts);

		try {
			const resp = await fetch(`https://api.telegram.org/bot${telegramToken}/${endpoint}`, {
				method: "POST",
				headers: { "Content-Type": `multipart/form-data; boundary=${formBoundary}` },
				body,
			});
			const rd = await resp.json() as any;
			if (rd.ok) {
				console.log(dim(`[telegram] Sent file: ${fileName}`));
			} else {
				console.error(dim(`[telegram] Failed to send file ${fileName}: ${rd.description}`));
			}
		} catch (err: any) {
			console.error(dim(`[telegram] File send error: ${err.message}`));
		}
	}

	function startTelegramPolling(agentDir: string, serverOpts: VoiceServerOptions) {
		if (telegramPolling) return;
		telegramPolling = true;
		console.log(dim("[voice] Telegram polling started"));

		async function poll() {
			if (!telegramPolling) return;
			try {
				const res = await fetch(
					`https://api.telegram.org/bot${telegramToken}/getUpdates?offset=${telegramOffset}&timeout=30&allowed_updates=["message"]`,
				);
				const data = await res.json() as any;
				if (data.ok && data.result) {
					for (const update of data.result) {
						telegramOffset = update.update_id + 1;
						const msg = update.message;
						if (!msg) continue;

						const chatId = msg.chat.id;
						lastTelegramChatId = chatId;
						const fromName = msg.from?.first_name || "User";
						const fromUsername = (msg.from?.username || "").toLowerCase();

						// Security: reject messages from unauthorized users
						// Empty = block all, * = allow all, otherwise check username list
						if (!telegramAllowedUsers.has("*")) {
							if (telegramAllowedUsers.size === 0 || !telegramAllowedUsers.has(fromUsername)) {
								console.log(dim(`[telegram] Blocked message from unauthorized user: @${fromUsername || "(no username)"} (${fromName})`));
								continue;
							}
						}

						let userText = msg.text || msg.caption || "";
						let imageContext = "";

						// Handle photo messages
						if (msg.photo && msg.photo.length > 0) {
							const largest = msg.photo[msg.photo.length - 1];
							const dl = await downloadTelegramFile(largest.file_id, agentDir);
							if (dl) {
								imageContext = ` [Image saved to ${dl.path}]`;
								// Notify browser of file change
								broadcastToBrowsers({ type: "files_changed" } as any);
							}
						}

						// Handle document/file messages
						if (msg.document) {
							const dl = await downloadTelegramFile(msg.document.file_id, agentDir);
							if (dl) {
								imageContext = ` [File saved to ${dl.path}: ${msg.document.file_name || dl.name}]`;
								broadcastToBrowsers({ type: "files_changed" } as any);
							}
						}

						if (!userText && !imageContext) continue;

						// ── Approval gate reply check ──
						if (userText && handleApprovalReply(userText)) {
							console.log(dim(`[telegram] Approval reply from ${fromName}: ${userText}`));
							const approvalMsg: ServerMessage = { type: "transcript", role: "user", text: `[Telegram] ${fromName}: ${userText}` };
							appendMessage(serverOpts.agentDir, activeBranch, approvalMsg);
							broadcastToBrowsers(approvalMsg);
							continue;
						}

						const fullText = `${userText}${imageContext}`.trim();
						console.log(dim(`[telegram] ${fromName}: ${fullText.slice(0, 100)}`));

						// ── Trigger check ──
						if (userText) {
							const trigger = matchTrigger(agentDir, "telegram", fromName, userText);
							if (trigger) {
								console.log(dim(`[triggers] Matched trigger ${trigger.id} for Telegram/${fromName}: "${userText.slice(0, 60)}" → "${trigger.reply.slice(0, 60)}"`));
								try {
									await fetch(`https://api.telegram.org/bot${telegramToken}/sendMessage`, {
										method: "POST",
										headers: { "Content-Type": "application/json" },
										body: JSON.stringify({ chat_id: chatId, text: trigger.reply }),
									});
									const triggerLog: ServerMessage = { type: "transcript", role: "assistant", text: `[Trigger → ${fromName}]: ${trigger.reply}` };
									appendMessage(serverOpts.agentDir, activeBranch, triggerLog);
									broadcastToBrowsers(triggerLog);
								} catch (err: any) {
									console.error(dim(`[triggers] Telegram auto-reply failed: ${err.message}`));
								}
								continue; // Skip agent processing for triggered messages
							}
						}

						// Save to shared chat history & broadcast to web UI
						const userMsg: ServerMessage = { type: "transcript", role: "user", text: `[Telegram] ${fromName}: ${fullText}` };
						appendMessage(serverOpts.agentDir, activeBranch, userMsg);
						broadcastToBrowsers(userMsg);

						// Send typing indicator
						await fetch(`https://api.telegram.org/bot${telegramToken}/sendChatAction`, {
							method: "POST",
							headers: { "Content-Type": "application/json" },
							body: JSON.stringify({ chat_id: chatId, action: "typing" }),
						}).catch(() => {});

						// Snapshot files before agent runs
						const beforeFiles = snapshotFiles(agentDir);

						// Run agent query
						try {
							const agentWorking: ServerMessage = { type: "agent_working", query: fullText };
							broadcastToBrowsers(agentWorking);
							appendMessage(serverOpts.agentDir, activeBranch, agentWorking);

							const tgContext = await getAgentContext(agentDir, activeBranch);
							const tgComposio = await getComposioContext(fullText);
							let tgSystemPrompt = "You are an AI assistant responding to a Telegram user. " +
								"Any files you create or modify will be AUTOMATICALLY sent back to the user on Telegram. " +
								"When asked to create documents (PDF, Word, PPT, spreadsheets, images, markdown files, text files, etc.), " +
								"write them to the workspace/ directory. The files will be delivered to the user immediately after you finish. " +
								"Keep text responses concise since they appear in a chat interface.";
							if (whatsappSock && whatsappConnected) {
								tgSystemPrompt += " You can also send WhatsApp messages to contacts using the send_whatsapp_message tool. " +
									"If you don't know a contact's number, ask the user or use list_whatsapp_contacts to check saved contacts.";
							}
							tgSystemPrompt += " You can set up auto-response triggers using create_trigger — e.g. 'when Kalps says hi on WhatsApp, reply hello friend'.";
							tgSystemPrompt += "\n\n" + getCurrentDateTimeContext();
							if (tgComposio.promptSuffix) tgSystemPrompt += "\n\n" + tgComposio.promptSuffix;
							if (tgContext) tgSystemPrompt += "\n\n" + tgContext;
							const tgTools = [
								...(whatsappSock && whatsappConnected ? createWhatsAppTools(whatsappSock, agentDir) : []),
								...createTriggerTools(agentDir),
								...tgComposio.tools,
							];
							const result = query({
								prompt: `[Telegram message from ${fromName}]: ${fullText}`,
								dir: agentDir,
								model: serverOpts.model,
								env: serverOpts.env,
								maxTurns: 10,
								systemPrompt: tgSystemPrompt,
								...(tgTools.length ? { tools: tgTools } : {}),
							});
							let reply = "";
							for await (const m of result) {
								if (m.type === "assistant" && m.content) reply += m.content;
								if (m.type === "tool_use") {
									const toolMsg: ServerMessage = { type: "tool_call", toolName: m.toolName, args: m.args ?? {} };
									appendMessage(serverOpts.agentDir, activeBranch, toolMsg);
								}
							}
							reply = reply.trim();

							// Save agent response to shared history & broadcast
							const doneMsg: ServerMessage = { type: "agent_done", result: reply.slice(0, 500) };
							appendMessage(serverOpts.agentDir, activeBranch, doneMsg);
							broadcastToBrowsers(doneMsg);

							const assistantMsg: ServerMessage = { type: "transcript", role: "assistant", text: reply };
							appendMessage(serverOpts.agentDir, activeBranch, assistantMsg);
							broadcastToBrowsers(assistantMsg);

							if (reply) {
								// Split long messages (Telegram 4096 char limit)
								const chunks: string[] = [];
								for (let i = 0; i < reply.length; i += 4096) {
									chunks.push(reply.slice(i, i + 4096));
								}
								for (const chunk of chunks) {
									await fetch(`https://api.telegram.org/bot${telegramToken}/sendMessage`, {
										method: "POST",
										headers: { "Content-Type": "application/json" },
										body: JSON.stringify({ chat_id: chatId, text: chunk, parse_mode: "Markdown" }),
									}).catch(async () => {
										// Fallback without Markdown if parsing fails
										await fetch(`https://api.telegram.org/bot${telegramToken}/sendMessage`, {
											method: "POST",
											headers: { "Content-Type": "application/json" },
											body: JSON.stringify({ chat_id: chatId, text: chunk }),
										}).catch(() => {});
									});
								}
							}

							// Detect new/modified files and send them back to Telegram
							const afterFiles = snapshotFiles(agentDir);
							const newFiles = diffSnapshots(beforeFiles, afterFiles);
							const filesToSend = newFiles.filter((f) => {
								const ext = f.split(".").pop()?.toLowerCase() || "";
								// Skip chat history, internal files, and non-sendable types
								if (f.startsWith(".gitagent/") || f.startsWith("node_modules/")) return false;
								if (f === ".env" || f === ".gitignore") return false;
								return SENDABLE_EXTS.has(ext);
							});

							for (const filePath of filesToSend) {
								// Send upload_document action for each file
								await fetch(`https://api.telegram.org/bot${telegramToken}/sendChatAction`, {
									method: "POST",
									headers: { "Content-Type": "application/json" },
									body: JSON.stringify({ chat_id: chatId, action: "upload_document" }),
								}).catch(() => {});
								await sendTelegramFile(chatId, filePath, agentDir, filePath.split("/").pop());
							}

							// Notify browser of any file changes from agent
							broadcastToBrowsers({ type: "files_changed" } as any);
						} catch (err: any) {
							console.error(dim(`[telegram] Agent error: ${err.message}`));
							await fetch(`https://api.telegram.org/bot${telegramToken}/sendMessage`, {
								method: "POST",
								headers: { "Content-Type": "application/json" },
								body: JSON.stringify({ chat_id: chatId, text: "Sorry, I encountered an error processing your message." }),
							}).catch(() => {});
						}
					}
				}
			} catch (err: any) {
				console.error(dim(`[telegram] Poll error: ${err.message}`));
			}
			if (telegramPolling) telegramPollTimer = setTimeout(poll, 500);
		}
		poll();
	}

	// Auto-connect if token is already configured
	if (telegramToken) {
		fetch(`https://api.telegram.org/bot${telegramToken}/getMe`)
			.then((r) => r.json() as Promise<any>)
			.then((d) => {
				if (d.ok) {
					telegramBotInfo = d.result;
					startTelegramPolling(agentRoot, opts);
					console.log(dim(`[voice] Telegram bot connected: @${d.result.username}`));
				}
			})
			.catch(() => {});
	}

	// ── WhatsApp state ─────────────────────────────────────────────────
	let lastWhatsAppJid: string | null = null;
	let whatsappSock: any = null;
	let whatsappConnected = false;
	let whatsappPhoneNumber: string | null = null;
	let whatsappQrCode: string | null = null;
	const whatsappSentIds = new Set<string>();

	// ── WhatsApp contacts store ────────────────────────────────────────
	interface WAContact { name: string; phone: string; jid: string }

	function contactsPath(agentDir: string): string {
		return join(agentDir, ".gitagent", "whatsapp-contacts.json");
	}

	function loadContacts(agentDir: string): WAContact[] {
		try { return JSON.parse(readFileSync(contactsPath(agentDir), "utf-8")); }
		catch { return []; }
	}

	function saveContacts(agentDir: string, contacts: WAContact[]): void {
		const dir = join(agentDir, ".gitagent");
		mkdirSync(dir, { recursive: true });
		writeFileSync(contactsPath(agentDir), JSON.stringify(contacts, null, 2));
	}

	function findContact(agentDir: string, nameQuery: string): WAContact | undefined {
		const q = nameQuery.toLowerCase();
		return loadContacts(agentDir).find(c => c.name.toLowerCase() === q || c.name.toLowerCase().includes(q));
	}

	function upsertContact(agentDir: string, contact: WAContact): void {
		const contacts = loadContacts(agentDir);
		const idx = contacts.findIndex(c => c.jid === contact.jid);
		if (idx >= 0) contacts[idx] = contact;
		else contacts.push(contact);
		saveContacts(agentDir, contacts);
	}

	/** Build WhatsApp tools that use the live Baileys socket */
	function createWhatsAppTools(sock: any, agentDir: string): GCToolDefinition[] {
		return [
			{
				name: "send_whatsapp_message",
				description: "Send a WhatsApp message to a contact. You can specify either a phone number (with country code, e.g. '919876543210') or a contact name (if previously saved). The message will be sent immediately.",
				inputSchema: {
					type: "object",
					properties: {
						to: { type: "string", description: "Contact name or phone number (with country code, no '+' prefix, e.g. '919876543210')" },
						message: { type: "string", description: "Message text to send" },
					},
					required: ["to", "message"],
				},
				handler: async (args: { to: string; message: string }) => {
					let jid: string;
					let displayName = args.to;

					// Try contact lookup first, then treat as phone number
					const contact = findContact(agentDir, args.to);
					if (contact) {
						jid = contact.jid;
						displayName = contact.name;
					} else {
						const digits = args.to.replace(/[^0-9]/g, "");
						if (!digits || digits.length < 7) {
							return `Contact "${args.to}" not found. Use save_whatsapp_contact to save them first, or provide a phone number with country code (e.g. 919876543210).`;
						}
						jid = `${digits}@s.whatsapp.net`;
					}

					const sent = await sock.sendMessage(jid, { text: args.message });
					if (sent?.key?.id) whatsappSentIds.add(sent.key.id);
					console.log(dim(`[whatsapp] Sent message to ${displayName} (${jid}): ${args.message.slice(0, 80)}`));
					return `Message sent to ${displayName}.`;
				},
			},
			{
				name: "save_whatsapp_contact",
				description: "Save a WhatsApp contact for future use. This lets you send messages by name instead of phone number.",
				inputSchema: {
					type: "object",
					properties: {
						name: { type: "string", description: "Contact name (e.g. 'Kalps')" },
						phone: { type: "string", description: "Phone number with country code, no '+' prefix (e.g. '919876543210')" },
					},
					required: ["name", "phone"],
				},
				handler: async (args: { name: string; phone: string }) => {
					const digits = args.phone.replace(/[^0-9]/g, "");
					const jid = `${digits}@s.whatsapp.net`;
					upsertContact(agentDir, { name: args.name, phone: digits, jid });
					console.log(dim(`[whatsapp] Saved contact: ${args.name} → ${digits}`));
					return `Contact "${args.name}" saved with phone ${digits}.`;
				},
			},
			{
				name: "list_whatsapp_contacts",
				description: "List all saved WhatsApp contacts.",
				inputSchema: { type: "object", properties: {} },
				handler: async () => {
					const contacts = loadContacts(agentDir);
					if (!contacts.length) return "No saved contacts. Use save_whatsapp_contact to add one.";
					return contacts.map(c => `${c.name}: ${c.phone}`).join("\n");
				},
			},
		];
	}

	// ── Message triggers ──────────────────────────────────────────────
	interface Trigger {
		id: string;
		from: string;       // contact name or "*" for anyone
		pattern: string;    // substring/regex to match in message
		reply: string;      // auto-reply text (if set, sends directly without agent)
		prompt?: string;    // optional: run agent with this prompt instead of static reply
		platform: string;   // "whatsapp" | "telegram" | "*"
		enabled: boolean;
	}

	function triggersPath(agentDir: string): string {
		return join(agentDir, ".gitagent", "triggers.json");
	}

	function loadTriggers(agentDir: string): Trigger[] {
		try { return JSON.parse(readFileSync(triggersPath(agentDir), "utf-8")); }
		catch { return []; }
	}

	function saveTriggers(agentDir: string, triggers: Trigger[]): void {
		const dir = join(agentDir, ".gitagent");
		mkdirSync(dir, { recursive: true });
		writeFileSync(triggersPath(agentDir), JSON.stringify(triggers, null, 2));
	}

	function matchTrigger(agentDir: string, platform: string, from: string, message: string): Trigger | undefined {
		const triggers = loadTriggers(agentDir);
		const fromLower = from.toLowerCase();
		const msgLower = message.toLowerCase();
		return triggers.find(t => {
			if (!t.enabled) return false;
			if (t.platform !== "*" && t.platform !== platform) return false;
			if (t.from !== "*") {
				// Match by contact name or phone number
				const contact = findContact(agentDir, t.from);
				if (contact) {
					if (fromLower !== contact.jid && fromLower !== contact.phone && fromLower !== contact.name.toLowerCase()) return false;
				} else if (fromLower !== t.from.toLowerCase()) return false;
			}
			// Pattern match — try regex first, fall back to substring
			try {
				if (new RegExp(t.pattern, "i").test(message)) return true;
			} catch {
				if (msgLower.includes(t.pattern.toLowerCase())) return true;
			}
			return false;
		});
	}

	function createTriggerTools(agentDir: string): GCToolDefinition[] {
		return [
			{
				name: "create_trigger",
				description: "Create an auto-response trigger. When a message matching the pattern arrives from the specified contact, the reply is sent automatically. Use from='*' to match anyone. Use platform='*' for all platforms.",
				inputSchema: {
					type: "object",
					properties: {
						from: { type: "string", description: "Contact name, phone number, or '*' for anyone" },
						pattern: { type: "string", description: "Text pattern to match (substring or regex)" },
						reply: { type: "string", description: "Auto-reply message to send" },
						platform: { type: "string", enum: ["whatsapp", "telegram", "*"], description: "Platform to trigger on (default: '*')" },
					},
					required: ["from", "pattern", "reply"],
				},
				handler: async (args: { from: string; pattern: string; reply: string; platform?: string }) => {
					const trigger: Trigger = {
						id: Date.now().toString(36),
						from: args.from,
						pattern: args.pattern,
						reply: args.reply,
						platform: args.platform || "*",
						enabled: true,
					};
					const triggers = loadTriggers(agentDir);
					triggers.push(trigger);
					saveTriggers(agentDir, triggers);
					console.log(dim(`[triggers] Created: when ${trigger.from} says "${trigger.pattern}" → "${trigger.reply}" (${trigger.platform})`));
					return `Trigger created (id: ${trigger.id}). When ${trigger.from} sends a message matching "${trigger.pattern}", I'll auto-reply: "${trigger.reply}"`;
				},
			},
			{
				name: "list_triggers",
				description: "List all message triggers.",
				inputSchema: { type: "object", properties: {} },
				handler: async () => {
					const triggers = loadTriggers(agentDir);
					if (!triggers.length) return "No triggers set up.";
					return triggers.map(t =>
						`[${t.id}] ${t.enabled ? "ON" : "OFF"} | from: ${t.from} | pattern: "${t.pattern}" | reply: "${t.reply}" | platform: ${t.platform}`
					).join("\n");
				},
			},
			{
				name: "delete_trigger",
				description: "Delete a trigger by its ID.",
				inputSchema: {
					type: "object",
					properties: { id: { type: "string", description: "Trigger ID to delete" } },
					required: ["id"],
				},
				handler: async (args: { id: string }) => {
					const triggers = loadTriggers(agentDir);
					const idx = triggers.findIndex(t => t.id === args.id);
					if (idx < 0) return `Trigger "${args.id}" not found.`;
					const removed = triggers.splice(idx, 1)[0];
					saveTriggers(agentDir, triggers);
					console.log(dim(`[triggers] Deleted: ${removed.id}`));
					return `Trigger "${removed.id}" deleted (was: ${removed.from} / "${removed.pattern}").`;
				},
			},
			{
				name: "toggle_trigger",
				description: "Enable or disable a trigger by its ID.",
				inputSchema: {
					type: "object",
					properties: {
						id: { type: "string", description: "Trigger ID" },
						enabled: { type: "boolean", description: "true to enable, false to disable" },
					},
					required: ["id", "enabled"],
				},
				handler: async (args: { id: string; enabled: boolean }) => {
					const triggers = loadTriggers(agentDir);
					const t = triggers.find(t => t.id === args.id);
					if (!t) return `Trigger "${args.id}" not found.`;
					t.enabled = args.enabled;
					saveTriggers(agentDir, triggers);
					return `Trigger "${t.id}" ${args.enabled ? "enabled" : "disabled"}.`;
				},
			},
		];
	}

	async function startWhatsApp(agentDir: string, serverOpts: VoiceServerOptions) {
		const {
			default: makeWASocket,
			useMultiFileAuthState,
			makeCacheableSignalKeyStore,
			fetchLatestBaileysVersion,
			DisconnectReason,
			jidNormalizedUser,
		} = await import("baileys");

		const authDir = join(agentDir, ".gitagent/whatsapp-auth");
		mkdirSync(authDir, { recursive: true });

		const { state, saveCreds } = await useMultiFileAuthState(authDir);
		const { version } = await fetchLatestBaileysVersion();

		const sock = makeWASocket({
			auth: { creds: state.creds, keys: makeCacheableSignalKeyStore(state.keys) },
			version,
			browser: ["GitClaw", "cli", "0.3.1"],
			printQRInTerminal: false,
			syncFullHistory: false,
			markOnlineOnConnect: false,
		});
		whatsappSock = sock;

		sock.ev.on("connection.update", (update: any) => {
			const { connection, lastDisconnect, qr } = update;
			if (qr) {
				whatsappQrCode = qr;
				broadcastToBrowsers({ type: "whatsapp_qr", qr } as any);
				console.log(dim("[whatsapp] QR code generated — scan with WhatsApp"));
			}
			if (connection === "open") {
				whatsappConnected = true;
				whatsappQrCode = null;
				const jid = sock.user?.id || "";
				whatsappPhoneNumber = jid.replace(/:.*@/, "@").replace("@s.whatsapp.net", "");
				console.log(dim(`[whatsapp] Connected: ${whatsappPhoneNumber}`));
				broadcastToBrowsers({ type: "whatsapp_status", connected: true, phoneNumber: whatsappPhoneNumber } as any);
			}
			if (connection === "close") {
				whatsappConnected = false;
				whatsappQrCode = null;
				const statusCode = (lastDisconnect?.error as any)?.output?.statusCode;
				const loggedOut = statusCode === DisconnectReason.loggedOut;
				console.log(dim(`[whatsapp] Disconnected (code=${statusCode}, loggedOut=${loggedOut})`));
				broadcastToBrowsers({ type: "whatsapp_status", connected: false } as any);
				if (!loggedOut) {
					// Auto-reconnect
					setTimeout(() => startWhatsApp(agentDir, serverOpts).catch(() => {}), 3000);
				}
			}
		});

		sock.ev.on("creds.update", saveCreds);

		sock.ev.on("messages.upsert", async ({ messages, type }: any) => {
			console.log(dim(`[whatsapp] upsert type=${type}, count=${messages.length}`));
			if (type !== "notify") return;

			const ownJid = sock.user?.id ? jidNormalizedUser(sock.user.id) : null;
			// Also track our LID (Linked Identity) — WhatsApp may route self-DMs via LID
			const ownLid = (sock as any).user?.lid?.replace(/:.*@/, "@") || null;
			if (!ownJid) return;

			for (const msg of messages) {
				console.log(dim(`[whatsapp] msg: remoteJid=${msg.key.remoteJid}, fromMe=${msg.key.fromMe}, ownJid=${ownJid}, ownLid=${ownLid}, id=${msg.key.id}`));
				// Skip agent's own replies
				if (whatsappSentIds.has(msg.key.id!)) continue;

				const incomingText = msg.message?.conversation
					|| msg.message?.extendedTextMessage?.text || "";
				if (!incomingText) continue;

				const senderJid = msg.key.remoteJid!;
				const isSelf = senderJid === ownJid || (ownLid && senderJid === ownLid);

				// ── Trigger check (runs on ALL incoming messages, not just self-DMs) ──
				if (!isSelf && !msg.key.fromMe) {
					// Resolve sender identity for trigger matching
					const senderPhone = senderJid.replace("@s.whatsapp.net", "");
					const senderContact = loadContacts(agentDir).find(c => c.jid === senderJid || c.phone === senderPhone);
					const senderName = senderContact?.name || senderPhone;

					const trigger = matchTrigger(agentDir, "whatsapp", senderContact?.name || senderJid, incomingText);
					if (trigger) {
						console.log(dim(`[triggers] Matched trigger ${trigger.id} for ${senderName}: "${incomingText.slice(0, 60)}" → "${trigger.reply.slice(0, 60)}"`));
						try {
							const sent = await sock.sendMessage(senderJid, { text: trigger.reply });
							if (sent?.key?.id) whatsappSentIds.add(sent.key.id);
							// Log to chat history
							const triggerLog: ServerMessage = { type: "transcript", role: "assistant", text: `[Trigger → ${senderName}]: ${trigger.reply}` };
							appendMessage(serverOpts.agentDir, activeBranch, triggerLog);
							broadcastToBrowsers(triggerLog);
						} catch (err: any) {
							console.error(dim(`[triggers] Failed to send auto-reply: ${err.message}`));
						}
					}
					continue; // Non-self messages are only processed for triggers
				}

				// Only process self-DMs for agent interaction
				if (!isSelf) continue;

				// ── Self-DM: full agent interaction ──
				const text = incomingText;
				const replyJid = senderJid;
				lastWhatsAppJid = replyJid;
				console.log(dim(`[whatsapp] Self-DM: ${text.slice(0, 100)}`));

				// ── Approval gate reply check ──
				if (handleApprovalReply(text)) {
					console.log(dim(`[whatsapp] Approval reply: ${text}`));
					const approvalMsg: ServerMessage = { type: "transcript", role: "user", text: `[WhatsApp]: ${text}` };
					appendMessage(serverOpts.agentDir, activeBranch, approvalMsg);
					broadcastToBrowsers(approvalMsg);
					continue;
				}

				// Broadcast to browser UI
				const userMsg: ServerMessage = { type: "transcript", role: "user", text: `[WhatsApp]: ${text}` };
				appendMessage(serverOpts.agentDir, activeBranch, userMsg);
				broadcastToBrowsers(userMsg);

				// Send typing presence
				try {
					await sock.presenceSubscribe(replyJid);
					await sock.sendPresenceUpdate("composing", replyJid);
				} catch { /* ignore */ }

				// Snapshot files before agent runs
				const beforeFiles = snapshotFiles(agentDir);

				try {
					const agentWorking: ServerMessage = { type: "agent_working", query: text };
					broadcastToBrowsers(agentWorking);
					appendMessage(serverOpts.agentDir, activeBranch, agentWorking);

					const waContext = await getAgentContext(agentDir, activeBranch);
					const waComposio = await getComposioContext(text);
					let waSystemPrompt = "You are an AI assistant responding via WhatsApp. " +
						"Any files you create or modify will be AUTOMATICALLY sent back to the user on WhatsApp. " +
						"When asked to create documents or markdown files, write them to the workspace/ directory. " +
						"Keep text responses concise since they appear in a chat interface. " +
						"You can send WhatsApp messages to other people using the send_whatsapp_message tool. " +
						"If you don't know a contact's number, ask the user or use list_whatsapp_contacts to check saved contacts. " +
						"You can also set up auto-response triggers using create_trigger — e.g. 'when Kalps says hi, reply hello friend'.";
					waSystemPrompt += "\n\n" + getCurrentDateTimeContext();
					if (waComposio.promptSuffix) waSystemPrompt += "\n\n" + waComposio.promptSuffix;
					if (waContext) waSystemPrompt += "\n\n" + waContext;
					const waTools = [...createWhatsAppTools(sock, agentDir), ...createTriggerTools(agentDir), ...waComposio.tools];
					const result = query({
						prompt: `[WhatsApp message]: ${text}`,
						dir: agentDir,
						model: serverOpts.model,
						env: serverOpts.env,
						maxTurns: 10,
						systemPrompt: waSystemPrompt,
						tools: waTools,
					});
					let reply = "";
					for await (const m of result) {
						if (m.type === "assistant" && m.content) reply += m.content;
					}
					reply = reply.trim();

					// Save agent response to shared history & broadcast
					const doneMsg: ServerMessage = { type: "agent_done", result: reply.slice(0, 500) };
					appendMessage(serverOpts.agentDir, activeBranch, doneMsg);
					broadcastToBrowsers(doneMsg);

					const assistantMsg: ServerMessage = { type: "transcript", role: "assistant", text: reply };
					appendMessage(serverOpts.agentDir, activeBranch, assistantMsg);
					broadcastToBrowsers(assistantMsg);

					// Send reply (chunk at 4000 chars for WhatsApp)
					if (reply) {
						const chunks: string[] = [];
						for (let i = 0; i < reply.length; i += 4000) chunks.push(reply.slice(i, i + 4000));
						for (const chunk of chunks) {
							const italicChunk = chunk.split("\n").map(line => line ? `_${line}_` : "").join("\n");
						const sent = await sock.sendMessage(replyJid, { text: `*GitClaw:*\n${italicChunk}` });
							if (sent?.key?.id) whatsappSentIds.add(sent.key.id);
						}
					}

					// Detect new/modified files and send them back
					const afterFiles = snapshotFiles(agentDir);
					const newFiles = diffSnapshots(beforeFiles, afterFiles).filter((f) => {
						const ext = f.split(".").pop()?.toLowerCase() || "";
						if (f.startsWith(".gitagent/") || f.startsWith("node_modules/")) return false;
						if (f === ".env" || f === ".gitignore") return false;
						return SENDABLE_EXTS.has(ext);
					});
					for (const filePath of newFiles) {
						const abs = join(agentDir, filePath);
						if (!existsSync(abs)) continue;
						const buffer = readFileSync(abs);
						const sent = await sock.sendMessage(replyJid, {
							document: buffer,
							fileName: filePath.split("/").pop() || "file",
							mimetype: "application/octet-stream",
						});
						if (sent?.key?.id) whatsappSentIds.add(sent.key.id);
					}

					broadcastToBrowsers({ type: "files_changed" } as any);
				} catch (err: any) {
					console.error(dim(`[whatsapp] Agent error: ${err.message}`));
					try {
						const sent = await sock.sendMessage(replyJid, { text: "*GitClaw:* _Sorry, I encountered an error processing your message._" });
						if (sent?.key?.id) whatsappSentIds.add(sent.key.id);
					} catch { /* ignore */ }
				}
			}
		});
	}

	function stopWhatsApp(clearAuth = false) {
		if (whatsappSock) {
			try { whatsappSock.end(undefined); } catch { /* ignore */ }
		}
		whatsappSock = null;
		whatsappConnected = false;
		whatsappPhoneNumber = null;
		whatsappQrCode = null;
		whatsappSentIds.clear();
		if (clearAuth) {
			const authDir = join(agentRoot, ".gitagent/whatsapp-auth");
			try { rmSync(authDir, { recursive: true, force: true }); } catch { /* ignore */ }
		}
	}

	// Auto-connect WhatsApp if auth exists
	const waAuthDir = join(agentRoot, ".gitagent/whatsapp-auth");
	if (existsSync(join(waAuthDir, "creds.json"))) {
		startWhatsApp(agentRoot, opts).catch(() => {});
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
		mtime?: number;
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
						result.push({ name, path: relPath, type: "file", mtime: st.mtimeMs });
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

	function escapeXml(s: string): string {
		return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
	}

	// ── Password protection ──────────────────────────────────────────
	const serverPassword = process.env.GITCLAW_PASSWORD || "";
	const authCookieName = "gitclaw_auth";

	function generateAuthToken(): string {
		// Simple hash of password + salt for cookie token
		const { createHash } = require("crypto") as typeof import("crypto");
		return createHash("sha256").update(serverPassword + "_gitclaw_session").digest("hex").slice(0, 32);
	}

	function isAuthenticated(req: IncomingMessage): boolean {
		if (!serverPassword) return true; // No password set — open access
		const cookie = req.headers.cookie || "";
		const match = cookie.match(new RegExp(`${authCookieName}=([^;]+)`));
		return match?.[1] === generateAuthToken();
	}

	const loginPageHtml = `<!DOCTYPE html>
<html><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1.0">
<title>GitClaw — Login</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
body{background:#0d1117;color:#e6edf3;font-family:'Inter',system-ui,sans-serif;display:flex;align-items:center;justify-content:center;height:100vh}
.login{background:#161b22;border:1px solid #30363d;border-radius:12px;padding:40px;width:340px;text-align:center}
.login h1{font-size:20px;margin-bottom:8px;font-weight:600}
.login p{font-size:13px;color:#8b949e;margin-bottom:24px}
.login input{width:100%;padding:12px 14px;background:#0d1117;border:1px solid #30363d;border-radius:6px;color:#e6edf3;font-size:14px;outline:none;margin-bottom:16px}
.login input:focus{border-color:#58a6ff}
.login button{width:100%;padding:12px;background:#238636;border:none;border-radius:6px;color:#fff;font-size:14px;font-weight:600;cursor:pointer}
.login button:hover{background:#2ea043}
.login .error{color:#f85149;font-size:12px;margin-bottom:12px;display:none}
</style></head><body>
<div class="login">
<h1>GitClaw</h1>
<p>Enter password to continue</p>
<div class="error" id="err">Incorrect password</div>
<form onsubmit="return doLogin()">
<input type="password" id="pw" placeholder="Password" autofocus>
<button type="submit">Login</button>
</form>
</div>
<script>
function doLogin(){
var pw=document.getElementById('pw').value;
fetch('/api/auth',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({password:pw})})
.then(function(r){return r.json().then(function(d){return{ok:r.ok,data:d};});})
.then(function(res){
if(res.ok&&res.data.ok){window.location.reload();}
else{document.getElementById('err').style.display='block';document.getElementById('pw').value='';}
});
return false;
}
</script></body></html>`;

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

		// ── Auth endpoints (always accessible) ──
		if (url.pathname === "/api/auth" && req.method === "POST") {
			const body = JSON.parse(await readBody(req));
			if (body.password === serverPassword) {
				res.setHeader("Set-Cookie", `${authCookieName}=${generateAuthToken()}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400`);
				jsonReply(res, 200, { ok: true });
			} else {
				jsonReply(res, 401, { ok: false, error: "Incorrect password" });
			}
			return;
		}

		// ── Password gate — block everything if not authenticated ──
		if (!isAuthenticated(req)) {
			if (url.pathname === "/health") {
				// Health check always open for load balancers
				jsonReply(res, 200, { status: "ok", auth: "required" });
				return;
			}
			res.writeHead(200, { "Content-Type": "text/html" });
			res.end(loginPageHtml);
			return;
		}

		if (url.pathname === "/health") {
			jsonReply(res, 200, { status: "ok" });

		} else if (url.pathname === "/api/vitals") {
			jsonReply(res, 200, getVitalsSnapshot());

		} else if (url.pathname === "/api/settings" && req.method === "GET") {
			// Read current model from agent.yaml and key presence from .env
			let model = "";
			try {
				const yamlRaw = readFileSync(join(agentRoot, "agent.yaml"), "utf-8");
				const m = yamlRaw.match(/preferred:\s*["']?([^"'\n]+)["']?/);
				if (m) model = m[1].trim();
			} catch { /* no agent.yaml */ }
			const keys: Record<string, boolean> = {};
			for (const k of ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "COMPOSIO_API_KEY"]) {
				keys[k] = !!process.env[k];
			}
			const baseUrl = process.env.GITCLAW_MODEL_BASE_URL || "";
			jsonReply(res, 200, { model, keys, baseUrl });

		} else if (url.pathname === "/api/settings" && req.method === "PUT") {
			try {
				const body = JSON.parse(await readBody(req));

				// Update .env with new keys
				const envPath = join(agentRoot, ".env");
				let envContent = "";
				try { envContent = readFileSync(envPath, "utf-8"); } catch { /* new file */ }

				const envKeys = body.keys || {};
				for (const [key, val] of Object.entries(envKeys)) {
					if (typeof val !== "string" || !val) continue;
					process.env[key] = val;
					const regex = new RegExp(`^${key}=.*$`, "m");
					if (regex.test(envContent)) {
						envContent = envContent.replace(regex, `${key}=${val}`);
					} else {
						envContent += (envContent.endsWith("\n") || !envContent ? "" : "\n") + `${key}=${val}\n`;
					}
				}
				writeFileSync(envPath, envContent, "utf-8");

				// Update model in agent.yaml
				if (body.model) {
					const yamlPath = join(agentRoot, "agent.yaml");
					try {
						let yamlContent = readFileSync(yamlPath, "utf-8");
						if (/preferred:\s*["']?[^"'\n]*["']?/.test(yamlContent)) {
							yamlContent = yamlContent.replace(
								/preferred:\s*["']?[^"'\n]*["']?/,
								`preferred: "${body.model}"`,
							);
						}
						writeFileSync(yamlPath, yamlContent, "utf-8");
					} catch { /* no agent.yaml to update */ }
				}

				// Update base URL in .env
				if (body.baseUrl !== undefined) {
					const baseUrlKey = "GITCLAW_MODEL_BASE_URL";
					if (body.baseUrl) {
						process.env[baseUrlKey] = body.baseUrl;
						const regex = new RegExp(`^${baseUrlKey}=.*$`, "m");
						if (regex.test(envContent)) {
							envContent = envContent.replace(regex, `${baseUrlKey}=${body.baseUrl}`);
						} else {
							envContent += (envContent.endsWith("\n") || !envContent ? "" : "\n") + `${baseUrlKey}=${body.baseUrl}\n`;
						}
					} else {
						delete process.env[baseUrlKey];
						envContent = envContent.replace(/^GITCLAW_MODEL_BASE_URL=.*\n?/m, "");
					}
					writeFileSync(envPath, envContent, "utf-8");
				}

				console.log("[settings] Configuration updated — keys in process.env, model in agent.yaml");
				jsonReply(res, 200, { ok: true });
			} catch (err: any) {
				jsonReply(res, 400, { error: err.message || "Invalid request" });
			}

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

		} else if (url.pathname === "/api/file/raw" && req.method === "GET") {
			// Serve raw file with correct MIME type (for images, etc.)
			const reqPath = url.searchParams.get("path");
			if (!reqPath) return jsonReply(res, 400, { error: "Missing path param" });
			const abs = safePath(reqPath);
			if (!abs) return jsonReply(res, 403, { error: "Path outside workspace" });
			if (!existsSync(abs)) return jsonReply(res, 404, { error: "File not found" });
			try {
				const st = statSync(abs);
				if (st.size > 10 * 1024 * 1024) return jsonReply(res, 413, { error: "File too large (>10MB)" });
				const ext = reqPath.split(".").pop()?.toLowerCase() || "";
				const mimeMap: Record<string, string> = {
					png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
					webp: "image/webp", svg: "image/svg+xml", ico: "image/x-icon", bmp: "image/bmp",
				};
				const mime = mimeMap[ext] || "application/octet-stream";
				const data = readFileSync(abs);
				res.writeHead(200, { "Content-Type": mime, "Content-Length": data.length, "Cache-Control": "no-cache" });
				res.end(data);
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

		// ── Telegram bot routes ─────────────────────────────────────────
		} else if (url.pathname === "/api/telegram/status" && req.method === "GET") {
			jsonReply(res, 200, {
				connected: telegramPolling,
				botName: telegramBotInfo?.first_name || null,
				botUsername: telegramBotInfo?.username || null,
				hasToken: !!telegramToken,
				allowedUsers: [...telegramAllowedUsers],
			});

		} else if (url.pathname === "/api/telegram/connect" && req.method === "POST") {
			const body = await readBody(req);
			try {
				const parsed = JSON.parse(body);
				if (parsed.token) telegramToken = parsed.token;
				if (parsed.allowedUsers !== undefined) {
					telegramAllowedUsers = new Set(
						(parsed.allowedUsers as string).split(",")
							.map((s: string) => s.trim().toLowerCase().replace(/^@/, ""))
							.filter(Boolean),
					);
				}
			} catch { /* use existing token */ }
			if (!telegramToken) return jsonReply(res, 400, { error: "No bot token provided" });

			// Save token + allowed users to .env for persistence
			const envPath = join(agentRoot, ".env");
			let envContent = "";
			try { envContent = readFileSync(envPath, "utf-8"); } catch { /* new file */ }

			// Save token
			if (envContent.includes("TELEGRAM_BOT_TOKEN=")) {
				envContent = envContent.replace(/^TELEGRAM_BOT_TOKEN=.*$/m, `TELEGRAM_BOT_TOKEN=${telegramToken}`);
			} else {
				envContent += `\nTELEGRAM_BOT_TOKEN=${telegramToken}\n`;
			}

			// Save allowed users
			const allowedStr = [...telegramAllowedUsers].join(",");
			if (envContent.includes("TELEGRAM_ALLOWED_USERS=")) {
				envContent = envContent.replace(/^TELEGRAM_ALLOWED_USERS=.*$/m, `TELEGRAM_ALLOWED_USERS=${allowedStr}`);
			} else if (allowedStr) {
				envContent += `TELEGRAM_ALLOWED_USERS=${allowedStr}\n`;
			}

			writeFileSync(envPath, envContent, "utf-8");

			// Validate token by calling getMe
			try {
				const meRes = await fetch(`https://api.telegram.org/bot${telegramToken}/getMe`);
				const meData = await meRes.json() as any;
				if (!meData.ok) return jsonReply(res, 400, { error: meData.description || "Invalid token" });
				telegramBotInfo = meData.result;

				// Start polling
				startTelegramPolling(agentRoot, opts);
				jsonReply(res, 200, { ok: true, botName: telegramBotInfo.first_name, botUsername: telegramBotInfo.username });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/telegram/allowed-users" && req.method === "POST") {
			const body = await readBody(req);
			try {
				const parsed = JSON.parse(body);
				telegramAllowedUsers = new Set(
					((parsed.users as string) || "").split(",")
						.map((s: string) => s.trim().toLowerCase().replace(/^@/, ""))
						.filter(Boolean),
				);
				// Persist to .env
				const envPath = join(agentRoot, ".env");
				let envContent = "";
				try { envContent = readFileSync(envPath, "utf-8"); } catch { /* new file */ }
				const allowedStr = [...telegramAllowedUsers].join(",");
				if (envContent.includes("TELEGRAM_ALLOWED_USERS=")) {
					envContent = envContent.replace(/^TELEGRAM_ALLOWED_USERS=.*$/m, `TELEGRAM_ALLOWED_USERS=${allowedStr}`);
				} else if (allowedStr) {
					envContent += `\nTELEGRAM_ALLOWED_USERS=${allowedStr}\n`;
				} else {
					envContent = envContent.replace(/^TELEGRAM_ALLOWED_USERS=.*\n?/m, "");
				}
				writeFileSync(envPath, envContent, "utf-8");
				jsonReply(res, 200, { ok: true, allowedUsers: [...telegramAllowedUsers] });
			} catch (err: any) {
				jsonReply(res, 400, { error: err.message });
			}

		} else if (url.pathname === "/api/telegram/disconnect" && req.method === "POST") {
			stopTelegramPolling();
			telegramBotInfo = null;
			jsonReply(res, 200, { ok: true });

		// ── WhatsApp routes ─────────────────────────────────────────────
		} else if (url.pathname === "/api/whatsapp/status" && req.method === "GET") {
			jsonReply(res, 200, {
				connected: whatsappConnected,
				phoneNumber: whatsappPhoneNumber,
				hasAuth: existsSync(join(agentRoot, ".gitagent/whatsapp-auth/creds.json")),
				qrCode: whatsappQrCode,
			});

		} else if (url.pathname === "/api/whatsapp/connect" && req.method === "POST") {
			if (whatsappConnected) return jsonReply(res, 200, { ok: true, connected: true, phoneNumber: whatsappPhoneNumber });
			try {
				await startWhatsApp(agentRoot, opts);
				jsonReply(res, 200, { ok: true, connecting: true });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/whatsapp/disconnect" && req.method === "POST") {
			let clearAuth = false;
			try {
				const body = await readBody(req);
				const parsed = JSON.parse(body);
				clearAuth = !!parsed.clearAuth;
			} catch { /* no body is fine */ }
			stopWhatsApp(clearAuth);
			jsonReply(res, 200, { ok: true });

		} else if (url.pathname === "/api/whatsapp/qr" && req.method === "GET") {
			jsonReply(res, 200, { qrCode: whatsappQrCode, connected: whatsappConnected });

		// ── Phone / Twilio webhook ──────────────────────────────────────
		} else if (url.pathname === "/api/phone/webhook" && req.method === "POST") {
			// Twilio sends SMS/voice webhooks here as application/x-www-form-urlencoded
			const body = await readBody(req);
			const params = new URLSearchParams(body);
			const from = params.get("From") || "";
			const smsBody = params.get("Body") || "";
			const callStatus = params.get("CallStatus") || "";

			if (smsBody) {
				// Incoming SMS
				console.log(dim(`[phone] SMS from ${from}: ${smsBody.slice(0, 100)}`));
				const userMsg: ServerMessage = { type: "transcript", role: "user", text: `[SMS ${from}]: ${smsBody}` };
				appendMessage(opts.agentDir, activeBranch, userMsg);
				broadcastToBrowsers(userMsg);

				// Check triggers
				const senderName = from.replace(/[^0-9]/g, "");
				const contact = loadContacts(opts.agentDir).find(c => c.phone === senderName || from.includes(c.phone));
				const trigger = matchTrigger(opts.agentDir, "phone", contact?.name || from, smsBody);

				if (trigger) {
					console.log(dim(`[triggers] Phone trigger ${trigger.id}: "${smsBody.slice(0, 40)}" → "${trigger.reply.slice(0, 40)}"`));
					// Reply with TwiML
					res.writeHead(200, { "Content-Type": "text/xml" });
					res.end(`<?xml version="1.0" encoding="UTF-8"?><Response><Message>${escapeXml(trigger.reply)}</Message></Response>`);
					const triggerLog: ServerMessage = { type: "transcript", role: "assistant", text: `[Trigger → ${from}]: ${trigger.reply}` };
					appendMessage(opts.agentDir, activeBranch, triggerLog);
					broadcastToBrowsers(triggerLog);
					return;
				}

				// Run agent for non-triggered messages
				try {
					const phoneContext = await getAgentContext(opts.agentDir, activeBranch);
					const phoneComposio = await getComposioContext(smsBody);
					let phoneSystemPrompt = "You are an AI assistant responding to an SMS message via Twilio. " +
						"Keep responses concise — SMS has character limits. Respond in plain text only.";
					phoneSystemPrompt += "\n\n" + getCurrentDateTimeContext();
					if (phoneComposio.promptSuffix) phoneSystemPrompt += "\n\n" + phoneComposio.promptSuffix;
					if (phoneContext) phoneSystemPrompt += "\n\n" + phoneContext;
					const phoneTools = [
						...createTriggerTools(opts.agentDir),
						...(whatsappSock && whatsappConnected ? createWhatsAppTools(whatsappSock, opts.agentDir) : []),
						...phoneComposio.tools,
					];
					const result = query({
						prompt: `[SMS from ${from}]: ${smsBody}`,
						dir: opts.agentDir,
						model: opts.model,
						env: opts.env,
						maxTurns: 5,
						systemPrompt: phoneSystemPrompt,
						...(phoneTools.length ? { tools: phoneTools } : {}),
					});
					let reply = "";
					for await (const m of result) {
						if (m.type === "assistant" && m.content) reply += m.content;
					}
					reply = reply.trim().slice(0, 1600); // SMS limit

					const assistantMsg: ServerMessage = { type: "transcript", role: "assistant", text: `[SMS → ${from}]: ${reply}` };
					appendMessage(opts.agentDir, activeBranch, assistantMsg);
					broadcastToBrowsers(assistantMsg);

					res.writeHead(200, { "Content-Type": "text/xml" });
					res.end(`<?xml version="1.0" encoding="UTF-8"?><Response><Message>${escapeXml(reply)}</Message></Response>`);
				} catch (err: any) {
					console.error(dim(`[phone] Agent error: ${err.message}`));
					res.writeHead(200, { "Content-Type": "text/xml" });
					res.end(`<?xml version="1.0" encoding="UTF-8"?><Response><Message>Sorry, something went wrong.</Message></Response>`);
				}
			} else if (callStatus) {
				// Voice call webhook — just acknowledge for now
				console.log(dim(`[phone] Call from ${from}, status: ${callStatus}`));
				res.writeHead(200, { "Content-Type": "text/xml" });
				res.end(`<?xml version="1.0" encoding="UTF-8"?><Response><Say>This number is managed by ${agentName}. Please send a text message instead.</Say></Response>`);
			} else {
				res.writeHead(200, { "Content-Type": "text/xml" });
				res.end(`<?xml version="1.0" encoding="UTF-8"?><Response></Response>`);
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

		// ── Chat branch API routes ──────────────────────────────────────
		} else if (url.pathname === "/api/chat/list" && req.method === "GET") {
			try {
				const git = (cmd: string) => execSync(cmd, { cwd: agentRoot, encoding: "utf-8" }).trim();
				const current = git("git rev-parse --abbrev-ref HEAD");
				// List branches matching chat/* pattern, plus the current branch
				let branches: string[];
				try {
					branches = git("git branch --list 'chat/*' --sort=-committerdate --format='%(refname:short)|%(committerdate:relative)'")
						.split("\n").filter(Boolean);
				} catch {
					branches = [];
				}
				const chats = branches.map((line) => {
					const [branch, time] = line.split("|");
					const name = branch.replace("chat/", "");
					return { branch, name, time: time || "" };
				});
				// If current branch is not a chat/* branch, add it at the top
				if (!current.startsWith("chat/")) {
					chats.unshift({ branch: current, name: current, time: "current" });
				}
				jsonReply(res, 200, { current, chats });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/chat/new" && req.method === "POST") {
			try {
				const git = (cmd: string) => execSync(cmd, { cwd: agentRoot, encoding: "utf-8" }).trim();
				// Generate branch name: chat/YYYY-MM-DD-HHMMSS
				const now = new Date();
				const pad = (n: number) => String(n).padStart(2, "0");
				const branch = `chat/${now.getFullYear()}${pad(now.getMonth()+1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
				// Stage and commit any pending changes on current branch
				try {
					git("git add -A");
					git('git commit -m "auto-save before new chat" --allow-empty');
				} catch {
					// No changes to commit, that's fine
				}
				// Create and switch to new branch
				git(`git checkout -b ${branch}`);
				activeBranch = branch;
				jsonReply(res, 200, { branch });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/chat/switch" && req.method === "POST") {
			try {
				const body = await readBody(req);
				const { branch } = JSON.parse(body);
				if (!branch) return jsonReply(res, 400, { error: "Missing branch" });
				const git = (cmd: string) => execSync(cmd, { cwd: agentRoot, encoding: "utf-8" }).trim();
				// Auto-save current branch
				try {
					git("git add -A");
					git('git commit -m "auto-save before switching chat" --allow-empty');
				} catch {}
				git(`git checkout ${branch}`);
				activeBranch = branch;
				jsonReply(res, 200, { branch });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/chat/delete" && req.method === "POST") {
			try {
				const body = await readBody(req);
				const { branch } = JSON.parse(body);
				if (!branch) return jsonReply(res, 400, { error: "Missing branch" });
				const git = (cmd: string) => execSync(cmd, { cwd: agentRoot, encoding: "utf-8" }).trim();
				const current = git("git rev-parse --abbrev-ref HEAD");
				if (branch === current) return jsonReply(res, 400, { error: "Cannot delete the active branch" });
				git(`git branch -D ${branch}`);
				deleteHistory(opts.agentDir, branch);
				jsonReply(res, 200, { ok: true });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

	} else if (url.pathname === "/api/chat/history" && req.method === "GET") {
			const branch = url.searchParams.get("branch");
			if (!branch) return jsonReply(res, 400, { error: "Missing branch param" });
			const messages = loadHistory(opts.agentDir, branch);
			jsonReply(res, 200, { branch, messages });

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

		// ── SkillFlows API ────────────────────────────────────────────
		} else if (url.pathname === "/api/skills/list" && req.method === "GET") {
			try {
				const skills = await discoverSkills(agentRoot);
				jsonReply(res, 200, { skills });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/flows/list" && req.method === "GET") {
			try {
				const workflows = await discoverWorkflows(agentRoot);
				const flows = workflows.filter((w) => w.type === "flow");
				jsonReply(res, 200, { flows });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/flows/save" && req.method === "POST") {
			const body = await readBody(req);
			let parsed: { name: string; description: string; steps: { skill: string; prompt: string; channel?: string }[] };
			try { parsed = JSON.parse(body); } catch { return jsonReply(res, 400, { error: "Invalid JSON" }); }
			if (!parsed.name || !parsed.steps?.length) return jsonReply(res, 400, { error: "Missing name or steps" });
			try {
				await saveFlowDefinition(agentRoot, parsed);
				jsonReply(res, 200, { ok: true });
			} catch (err: any) {
				jsonReply(res, 400, { error: err.message });
			}

		} else if (url.pathname === "/api/flows/delete" && req.method === "DELETE") {
			const body = await readBody(req);
			let parsed: { name: string };
			try { parsed = JSON.parse(body); } catch { return jsonReply(res, 400, { error: "Invalid JSON" }); }
			if (!parsed.name) return jsonReply(res, 400, { error: "Missing name" });
			try {
				await deleteFlowDefinition(agentRoot, parsed.name);
				jsonReply(res, 200, { ok: true });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		// ── Scheduler API ──────────────────────────────────────────────
		} else if (url.pathname === "/api/schedules/list" && req.method === "GET") {
			try {
				const schedules = await discoverSchedules(agentRoot);
				jsonReply(res, 200, { schedules });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/schedules/save" && req.method === "POST") {
			const body = await readBody(req);
			let parsed: { id: string; prompt: string; cron?: string; mode?: string; runAt?: string; enabled?: boolean };
			try { parsed = JSON.parse(body); } catch { return jsonReply(res, 400, { error: "Invalid JSON" }); }
			if (!parsed.id || !parsed.prompt) return jsonReply(res, 400, { error: "Missing id or prompt" });
			const mode = parsed.mode === "once" ? "once" as const : "repeat" as const;
			if (mode === "once" && parsed.runAt) {
				// runAt mode — validate the datetime is in the future
				const runAtDate = new Date(parsed.runAt);
				if (isNaN(runAtDate.getTime())) return jsonReply(res, 400, { error: "Invalid runAt datetime" });
			} else {
				// cron mode — validate expression
				if (!parsed.cron) return jsonReply(res, 400, { error: "Missing cron expression" });
				if (!cron.validate(parsed.cron)) return jsonReply(res, 400, { error: "Invalid cron expression" });
			}
			try {
				await saveSchedule(agentRoot, {
					id: parsed.id,
					prompt: parsed.prompt,
					cron: parsed.cron || "",
					mode,
					...(parsed.runAt ? { runAt: parsed.runAt } : {}),
					enabled: parsed.enabled !== false,
					createdAt: new Date().toISOString(),
				});
				await reloadSchedules(schedulerOpts);
				jsonReply(res, 200, { ok: true });
			} catch (err: any) {
				jsonReply(res, 400, { error: err.message });
			}

		} else if (url.pathname === "/api/schedules/delete" && req.method === "DELETE") {
			const body = await readBody(req);
			let parsed: { id: string };
			try { parsed = JSON.parse(body); } catch { return jsonReply(res, 400, { error: "Invalid JSON" }); }
			if (!parsed.id) return jsonReply(res, 400, { error: "Missing id" });
			try {
				await deleteSchedule(agentRoot, parsed.id);
				await reloadSchedules(schedulerOpts);
				jsonReply(res, 200, { ok: true });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/schedules/toggle" && req.method === "POST") {
			const body = await readBody(req);
			let parsed: { id: string; enabled: boolean };
			try { parsed = JSON.parse(body); } catch { return jsonReply(res, 400, { error: "Invalid JSON" }); }
			if (!parsed.id) return jsonReply(res, 400, { error: "Missing id" });
			try {
				await updateScheduleMeta(agentRoot, parsed.id, { enabled: parsed.enabled });
				await reloadSchedules(schedulerOpts);
				jsonReply(res, 200, { ok: true });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/schedules/run" && req.method === "POST") {
			const body = await readBody(req);
			let parsed: { id: string };
			try { parsed = JSON.parse(body); } catch { return jsonReply(res, 400, { error: "Invalid JSON" }); }
			if (!parsed.id) return jsonReply(res, 400, { error: "Missing id" });
			try {
				const schedules = await discoverSchedules(agentRoot);
				const schedule = schedules.find((s) => s.id === parsed.id);
				if (!schedule) return jsonReply(res, 404, { error: "Schedule not found" });
				jsonReply(res, 200, { ok: true, message: "Job triggered" });
				executeScheduledJob(schedule, schedulerOpts);
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else if (url.pathname === "/api/schedules/logs" && req.method === "GET") {
			const id = url.searchParams.get("id");
			if (!id) return jsonReply(res, 400, { error: "Missing id param" });
			try {
				const logFile = join(agentRoot, ".gitagent", "schedule-logs", `${id}.jsonl`);
				const raw = readFileSync(logFile, "utf-8");
				const lines = raw.trim().split("\n").filter(Boolean);
				const entries = lines.slice(-50).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
				jsonReply(res, 200, { entries });
			} catch {
				jsonReply(res, 200, { entries: [] });
			}

		// ── Skills Marketplace proxy ────────────────────────────────────
		} else if (url.pathname === "/api/skills-mp/proxy" && req.method === "GET") {
			const proxyPath = url.searchParams.get("path") || "/";
			// Forward all query params except "path" to skills.sh
			const forwardParams = new URLSearchParams(url.searchParams);
			forwardParams.delete("path");
			const qs = forwardParams.toString();
			const targetUrl = `https://skills.sh${proxyPath.startsWith("/") ? proxyPath : "/" + proxyPath}${qs ? (proxyPath.includes("?") ? "&" : "?") + qs : ""}`;

			try {
				const proxyRes = await fetch(targetUrl, {
					headers: {
						"User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
						"Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
					},
					redirect: "follow",
				});

				const contentType = proxyRes.headers.get("content-type") || "";

				// Non-HTML resources: pass through directly
				if (!contentType.includes("text/html")) {
					const buffer = Buffer.from(await proxyRes.arrayBuffer());
					res.writeHead(proxyRes.status, {
						"Content-Type": contentType,
						"Cache-Control": proxyRes.headers.get("cache-control") || "public, max-age=3600",
						"Access-Control-Allow-Origin": "*",
					});
					res.end(buffer);
					return;
				}

				let html = await proxyRes.text();

				// Rewrite relative src/href to absolute skills.sh URLs so assets load correctly
				// (Do NOT rewrite href for navigation links — that breaks React hydration.
				//  Navigation is handled by client-side click/history interception instead.)
				html = html.replace(/src="\/(?!\/)/g, 'src="https://skills.sh/');
				html = html.replace(/src='\/(?!\/)/g, "src='https://skills.sh/");
				// Rewrite stylesheet/preload hrefs to load from skills.sh
				html = html.replace(/href="\/_(next|static)\//g, 'href="https://skills.sh/_$1/');
				html = html.replace(/href='\/_(next|static)\//g, "href='https://skills.sh/_$1/");

				// Inject our custom script before </body>
				const injectedScript = `
<script>
(function() {
  const PROXY_BASE = '/api/skills-mp/proxy?path=';

  // Build a proxy URL that keeps query params as top-level params
  // so the server can forward them (e.g. ?q=docker) to skills.sh.
  // Without this, "/?q=docker" becomes "proxy?path=/?q=docker" and
  // the q param is buried inside the path value — nuqs/Next.js
  // cannot read it back from location.search, breaking search.
  function proxyUrl(rawUrl) {
    var qi = rawUrl.indexOf('?');
    if (qi === -1) return PROXY_BASE + rawUrl;
    var pathname = rawUrl.slice(0, qi);
    var qs = rawUrl.slice(qi + 1);           // e.g. "q=docker&view=all"
    return PROXY_BASE + encodeURIComponent(pathname) + '&' + qs;
  }

  // Fetch installed skills from lock file
  var __installedSkills = new Set();
  var __installedSources = new Set();
  fetch('/api/skills-mp/installed').then(function(r){return r.json();}).then(function(d){
    if(d&&d.installed) __installedSkills = new Set(d.installed);
    if(d&&d.sources) __installedSources = new Set(d.sources);
    processSkillButtons();
  }).catch(function(){});

  // Intercept fetch to route API calls to skills.sh (including full-origin URLs from Next.js)
  const _fetch = window.fetch;
  // e.g. fetch('http://localhost:3000/_next/data/...')
  window.fetch = function(url, opts) {
    if (typeof url === 'string') {
      if (url.startsWith('/') && !url.startsWith('//') && !url.startsWith('/api/skills-mp/')) {
        url = proxyUrl(url);
      } else if (url.startsWith(location.origin + '/')) {
        var path = url.slice(location.origin.length);
        if (!path.startsWith('/api/skills-mp/')) {
          url = proxyUrl(path);
        }
      }
    } else if (url instanceof Request) {
      var rUrl = url.url;
      if (rUrl.startsWith(location.origin + '/')) {
        var rPath = rUrl.slice(location.origin.length);
        if (!rPath.startsWith('/api/skills-mp/')) {
          url = new Request(proxyUrl(rPath), url);
        }
      }
    }
    return _fetch.call(this, url, opts);
  };

  // Intercept XMLHttpRequest.open
  const _xhrOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function(method, url) {
    var args = Array.prototype.slice.call(arguments, 2);
    if (typeof url === 'string') {
      if (url.startsWith('/') && !url.startsWith('//') && !url.startsWith('/api/skills-mp/')) {
        url = proxyUrl(url);
      } else if (url.startsWith(location.origin + '/')) {
        var path = url.slice(location.origin.length);
        if (!path.startsWith('/api/skills-mp/')) {
          url = proxyUrl(path);
        }
      }
    }
    return _xhrOpen.apply(this, [method, url].concat(args));
  };

  // Intercept history.pushState/replaceState for Next.js client-side navigation
  var _pushState = history.pushState;
  var _replaceState = history.replaceState;
  function rewriteHistoryUrl(originalFn, data, title, url) {
    if (typeof url === 'string' && url.startsWith('/') && !url.startsWith('//') && !url.startsWith('/api/skills-mp/')) {
      url = proxyUrl(url);
    }
    return originalFn.call(history, data, title, url);
  }
  history.pushState = function(data, title, url) { return rewriteHistoryUrl(_pushState, data, title, url); };
  history.replaceState = function(data, title, url) { return rewriteHistoryUrl(_replaceState, data, title, url); };

  // Intercept form submissions
  document.addEventListener('submit', function(e) {
    var form = e.target;
    if (!form || !form.action) return;
    var action = form.getAttribute('action');
    if (action && action.startsWith('/') && !action.startsWith('//') && !action.startsWith('/api/skills-mp/')) {
      e.preventDefault();
      var params = new URLSearchParams(new FormData(form)).toString();
      window.location.href = proxyUrl(action + (params ? (action.includes('?') ? '&' : '?') + params : ''));
    }
  }, true);

  function processSkillButtons() {
    // Find elements with npx skills add commands
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const textNodes = [];
    while (walker.nextNode()) textNodes.push(walker.currentNode);

    for (const node of textNodes) {
      const match = node.textContent && node.textContent.match(/npx\\s+skills\\s+add\\s+(\\S+)/);
      if (!match) continue;

      // Find the closest interactive parent (button, code block, pre)
      let target = node.parentElement;
      while (target && !['BUTTON','PRE','CODE','DIV'].includes(target.tagName)) {
        target = target.parentElement;
      }
      if (!target || target.dataset.gcProcessed) continue;
      target.dataset.gcProcessed = 'true';

      const source = match[1].replace(/^https:\\/\\/github\\.com\\//, '');
      const alreadyInstalled = __installedSources.has(source) || __installedSkills.has(source.split('/').pop());
      const btn = document.createElement('button');
      if (alreadyInstalled) {
        btn.textContent = 'Installed';
        btn.disabled = true;
        btn.style.cssText = 'background:#1a7f37;color:#fff;border:none;padding:8px 16px;border-radius:6px;cursor:default;font-size:14px;font-weight:600;margin:4px;opacity:0.85;';
      } else {
        btn.textContent = 'Install on GitClaw';
        btn.style.cssText = 'background:#238636;color:#fff;border:none;padding:8px 16px;border-radius:6px;cursor:pointer;font-size:14px;font-weight:600;margin:4px;';
        btn.onmouseenter = function(){ btn.style.background='#2ea043'; };
        btn.onmouseleave = function(){ btn.style.background='#238636'; };
        btn.onclick = function(e) {
          e.preventDefault();
          e.stopPropagation();
          btn.disabled = true;
          btn.textContent = 'Installing...';
          window.parent.postMessage({ type: 'install_skill', source: source }, '*');
        };
      }
      target.parentElement.insertBefore(btn, target.nextSibling);
    }
  }

  window.addEventListener('message', function(msg) {
    if (!msg.data || msg.data.type !== 'install_success') return;
    var btns = document.querySelectorAll('button');
    btns.forEach(function(b) {
      if (b.textContent === 'Installing...') {
        b.textContent = 'Installed';
        b.style.background = '#1a7f37';
        b.style.cursor = 'default';
        b.style.opacity = '0.85';
      }
    });
    // Refresh installed set so future processSkillButtons calls are up to date
    fetch('/api/skills-mp/installed').then(function(r){return r.json();}).then(function(d){
      if(d&&d.installed) __installedSkills = new Set(d.installed);
      if(d&&d.sources) __installedSources = new Set(d.sources);
    }).catch(function(){});
  });

  // Intercept link clicks to route through proxy
  document.addEventListener('click', function(e) {
    const a = e.target.closest('a');
    if (!a) return;
    const href = a.getAttribute('href');
    if (!href) return;
    // Already proxied
    if (href.startsWith('/api/skills-mp/proxy')) return;
    // Internal skills.sh links
    if (href.startsWith('/') && !href.startsWith('//')) {
      e.preventDefault();
      window.location.href = proxyUrl(href);
    } else if (href.startsWith('https://skills.sh/') || href.startsWith('https://skillsmp.com/')) {
      e.preventDefault();
      var parsed = new URL(href);
      window.location.href = proxyUrl(parsed.pathname + parsed.search);
    }
  }, true);

  // Run on load + observe for SPA changes
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', processSkillButtons);
  } else {
    processSkillButtons();
  }
  new MutationObserver(function() { processSkillButtons(); })
    .observe(document.body || document.documentElement, { childList: true, subtree: true });
})();
</script>`;

				html = html.replace(/<\/body>/i, injectedScript + "</body>");

				res.writeHead(200, { "Content-Type": "text/html; charset=utf-8", "Access-Control-Allow-Origin": "*" });
				res.end(html);
			} catch (err: any) {
				// Fallback if skills.sh is unreachable
				res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
				res.end(`<!DOCTYPE html>
<html><head><style>body{background:#0d1117;color:#c9d1d9;font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;text-align:center;}
a{color:#58a6ff;}</style></head>
<body><div><h2>Skills Marketplace Unavailable</h2><p>Could not reach skills.sh: ${err.message}</p>
<p><a href="https://skills.sh" target="_blank">Open skills.sh in a new tab</a></p></div></body></html>`);
			}

		// ── Skills Marketplace installed list ────────────────────────────
		} else if (url.pathname === "/api/skills-mp/installed" && req.method === "GET") {
			try {
				const lockPath = join(agentRoot, "skills-lock.json");
				if (existsSync(lockPath)) {
					const lock = JSON.parse(readFileSync(lockPath, "utf-8"));
					const skills = lock.skills || {};
					const names = Object.keys(skills);
					// Build a set of sources (repo slugs) that have installed skills
					const sources = [...new Set(Object.values(skills).map((s: any) => s.source))];
					jsonReply(res, 200, { installed: names, sources });
				} else {
					jsonReply(res, 200, { installed: [], sources: [] });
				}
			} catch (err: any) {
				jsonReply(res, 200, { installed: [], sources: [] });
			}

		// ── Skills Marketplace install ──────────────────────────────────
		} else if (url.pathname === "/api/skills-mp/install" && req.method === "POST") {
			let body = "";
			for await (const chunk of req) body += chunk;
			try {
				const { source } = JSON.parse(body) as { source: string };
				if (!source) return jsonReply(res, 400, { error: "Missing source" });

				// Shell out to the skills CLI — it handles all install logic
				const cleanSource = source.replace(/^https?:\/\/github\.com\//, "");
				const skillsDir = join(agentRoot, "skills");
				const before = new Set(existsSync(skillsDir) ? readdirSync(skillsDir) : []);

				execSync(`npx -y skills add -y ${cleanSource} --agent openclaw`, {
					cwd: agentRoot,
					encoding: "utf-8",
					timeout: 120000,
				});

				// Detect which skill directories were added (symlinked into skills/)
				const after = existsSync(skillsDir) ? readdirSync(skillsDir) : [];
				const added = after.filter(d => !before.has(d));
				const skillNames = added.length ? added : [cleanSource.split("/")[1] || cleanSource];
				console.log(dim(`[voice] Installed skill(s): ${skillNames.join(", ")} via npx skills add`));
				broadcastToBrowsers({ type: "files_changed" } as any);
				jsonReply(res, 200, { ok: true, skillName: skillNames.join(", "), path: `skills/`, installed: skillNames });
			} catch (err: any) {
				jsonReply(res, 500, { error: err.message });
			}

		} else {
			res.writeHead(404);
			res.end();
		}
	});

	// WebSocket server — adapter-agnostic proxy
	const wss = new WebSocketServer({ server: httpServer });

	wss.on("connection", async (browserWs: WS, req: IncomingMessage) => {
		// Check auth on WebSocket connections
		if (!isAuthenticated(req)) {
			browserWs.close(4401, "Unauthorized");
			return;
		}
		console.log(dim("[voice] Browser connected"));

		// ── Per-connection frame buffer + moment capture state ──────────
		let latestVideoFrame: { frame: string; mimeType: string; ts: number } | null = null;
		let lastFrameWriteTs = 0;
		let latestScreenFrame: { frame: string; mimeType: string; ts: number } | null = null;
		let lastScreenWriteTs = 0;
		let lastMomentCaptureTs = 0;
		const FRAME_WRITE_INTERVAL = 2000; // Write temp frame to disk every 2s
		const MOMENT_COOLDOWN = 60000;     // 60s between auto-captures
		const moodCounts: MoodCounts = { happy: 0, frustrated: 0, curious: 0, excited: 0, calm: 0 };
		let sessionMessageCount = 0;

		// Inject shared context (memory + conversation summary) into voice LLM instructions
		const voiceContext = await getVoiceContext(opts.agentDir, activeBranch);
		let instructions = opts.adapterConfig.instructions || "";
		if (voiceContext) {
			instructions += "\n\n" + voiceContext;
		}

		// Inject Composio awareness into adapter instructions so the voice LLM
		// never tells the user "I can't access" external services
		const adapterOpts = composioAdapter ? {
			...opts,
			adapterConfig: {
				...opts.adapterConfig,
				instructions: instructions +
					" The agent has FULL access to external services via Composio — Gmail, Google Calendar, GitHub, Slack, and more. " +
					"When the user asks to send emails, check calendars, or interact with any external service, ALWAYS use run_agent to handle it. " +
					"NEVER say you can't access these services or that you don't have these tools. The agent has them. Just call run_agent.",
			},
		} : {
			...opts,
			adapterConfig: {
				...opts.adapterConfig,
				instructions,
			},
		};
		let adapter: MultimodalAdapter | null = opts.adapterConfig.apiKey ? createAdapter(adapterOpts) : null;
		const sendToBrowser = (msg: ServerMessage) => {
			safeSend(browserWs, JSON.stringify(msg));
			appendMessage(opts.agentDir, activeBranch, msg);
			// Track mood from user transcripts
			if (msg.type === "transcript" && msg.role === "user" && !msg.partial) {
				sessionMessageCount++;
				const mood = detectMood(msg.text);
				if (mood) moodCounts[mood]++;
			}
			// Detect personal info in voice transcripts and save to memory
			if (msg.type === "transcript" && msg.role === "user" && !msg.partial && isMemoryWorthy(msg.text)) {
				saveMemoryInBackground(msg.text, opts.agentDir, opts.model, opts.env, () => {
					broadcastToBrowsers({ type: "memory_saving", status: "start", text: msg.text.slice(0, 60) });
				}, () => {
					broadcastToBrowsers({ type: "memory_saving", status: "done" });
					safeSend(browserWs, JSON.stringify({ type: "files_changed" }));
				});
			}
			// Auto-capture photo on memorable moments (with 60s cooldown)
			if (msg.type === "transcript" && msg.role === "user" && !msg.partial && isMomentWorthy(msg.text)) {
				const now = Date.now();
				if (now - lastMomentCaptureTs >= MOMENT_COOLDOWN) {
					lastMomentCaptureTs = now;
					// Use buffered frame if available and fresh (<5s)
					let frameBuffer: Buffer | undefined;
					if (latestVideoFrame && (now - latestVideoFrame.ts) < 5000) {
						frameBuffer = Buffer.from(latestVideoFrame.frame, "base64");
					}
					capturePhoto(agentRoot, msg.text.slice(0, 60), frameBuffer).catch((err) => {
						console.error(dim(`[voice] Auto photo capture failed: ${err.message}`));
					});
				}
			}
		};

		if (adapter) {
			try {
				await adapter.connect({
					toolHandler: createToolHandler(sendToBrowser),
					onMessage: sendToBrowser,
				});
				console.log(dim(`[voice] Adapter ready (${opts.adapter})`));
			} catch (err: any) {
				console.error(dim(`[voice] Adapter connection failed: ${err.message}`));
				safeSend(browserWs, JSON.stringify({ type: "error", message: `Voice connection failed: ${err.message}` }));
				adapter = null; // Fall back to text-only
			}
		}
		if (!adapter) {
			safeSend(browserWs, JSON.stringify({
				type: "transcript", role: "assistant",
				text: "Voice mode unavailable — no API key set. You can still chat via text.",
			}));
		}

		// Parse browser messages into ClientMessage and forward to adapter
		browserWs.on("message", async (data) => {
			try {
				const msg = JSON.parse(data.toString()) as ClientMessage;

				// Buffer video frames and throttle-write to disk for capture_photo tool
				if (msg.type === "video_frame") {
					const source = msg.source || "camera";
					if (source === "screen") {
						latestScreenFrame = { frame: msg.frame, mimeType: msg.mimeType, ts: Date.now() };
						const now = Date.now();
						if (now - lastScreenWriteTs >= 3000) {
							lastScreenWriteTs = now;
							const frameBuffer = Buffer.from(msg.frame, "base64");
							const framePath = join(agentRoot, LATEST_SCREEN_FILE);
							writeFile(framePath, frameBuffer).catch(() => {});
						}
					} else {
						latestVideoFrame = { frame: msg.frame, mimeType: msg.mimeType, ts: Date.now() };
						const now = Date.now();
						if (now - lastFrameWriteTs >= FRAME_WRITE_INTERVAL) {
							lastFrameWriteTs = now;
							const frameBuffer = Buffer.from(msg.frame, "base64");
							const framePath = join(agentRoot, LATEST_FRAME_FILE);
							writeFile(framePath, frameBuffer).catch(() => {});
						}
					}
				}

				if (msg.type === "text") {
					appendMessage(opts.agentDir, activeBranch, { type: "transcript", role: "user", text: msg.text });

					// Detect @flow-name triggers
					const flowMatch = msg.text.match(/@([a-z0-9]+(?:-[a-z0-9]+)*)/);
					if (flowMatch) {
						try {
							const workflows = await discoverWorkflows(agentRoot);
							const flow = workflows.find((f) => f.name === flowMatch[1] && f.type === "flow");
							if (flow) {
								const userContext = msg.text.replace(/@[a-z0-9-]+/, "").trim();
								executeFlow(flow.name, userContext, sendToBrowser).catch((err) => {
									sendToBrowser({ type: "transcript", role: "assistant", text: `Flow error: ${err.message}` });
								});
								return; // skip adapter.send()
							}
						} catch {
							// Fall through to normal send if flow detection fails
						}
					}

					// Detect personal info and save to memory in background
					if (isMemoryWorthy(msg.text)) {
						saveMemoryInBackground(msg.text, opts.agentDir, opts.model, opts.env, () => {
							broadcastToBrowsers({ type: "memory_saving", status: "start", text: msg.text.slice(0, 60) });
						}, () => {
							broadcastToBrowsers({ type: "memory_saving", status: "done" });
							safeSend(browserWs, JSON.stringify({ type: "files_changed" }));
						});
					}

					// Text-only mode — call agent directly when no voice adapter
					if (!adapter) {
						const handler = createToolHandler(sendToBrowser);
						handler(msg.text).then((result) => {
							safeSend(browserWs, JSON.stringify({ type: "agent_done", result }));
							appendMessage(opts.agentDir, activeBranch, { type: "transcript", role: "assistant", text: result });
							safeSend(browserWs, JSON.stringify({ type: "files_changed" }));
						}).catch((err: any) => {
							safeSend(browserWs, JSON.stringify({ type: "error", message: err.message }));
						});
						return;
					}
				} else if (msg.type === "file") {
					// Save uploaded file to disk so the text agent can use it
					const uploadsDir = join(agentRoot, "workspace");
					mkdirSync(uploadsDir, { recursive: true });
					const safeName = (msg as any).name.replace(/[^a-zA-Z0-9._-]/g, "_");
					const filePath = join(uploadsDir, safeName);
					writeFileSync(filePath, Buffer.from((msg as any).data, "base64"));
					const relPath = relative(agentRoot, filePath);
					console.log(dim(`[voice] Saved uploaded file: ${relPath}`));

					// Inject path into message so voice LLM tells the agent where the file is
					const userText = (msg as any).text || "";
					(msg as any).text = `${userText}${userText ? " " : ""}[File saved to: ${relPath} (absolute: ${filePath})]`;

					appendMessage(opts.agentDir, activeBranch, {
						type: "transcript", role: "user",
						text: `${userText} [Attached: ${safeName} → ${relPath}]`.trim(),
					});
				}
				adapter?.send(msg);
			} catch {
				// Ignore unparseable messages
			}
		});

		browserWs.on("close", () => {
			console.log(dim("[voice] Browser disconnected"));
			adapter?.disconnect().catch(() => {});
			// Summarize chat history, save mood, and write journal — track promises for graceful shutdown
			const p = Promise.allSettled([
				summarizeHistory(opts.agentDir, activeBranch).catch((err) => {
					console.error(dim(`[voice] Background summarization failed: ${err.message}`));
				}),
				saveMoodEntry(opts.agentDir, moodCounts, sessionMessageCount).catch((err) => {
					console.error(dim(`[voice] Mood save failed: ${err.message}`));
				}),
				writeJournalEntry(opts.agentDir, activeBranch, moodCounts, opts.model, opts.env).catch((err) => {
					console.error(dim(`[voice] Journal write failed: ${err.message}`));
				}),
			]);
			pendingShutdownWork.push(p);
		});
	});

	await new Promise<void>((resolve) => {
		httpServer.listen(port, () => resolve());
	});

	console.log(bold(`Voice server running on :${port}`));
	console.log(dim(`[voice] Backend: ${opts.adapter}`));
	console.log(dim(`[voice] Open http://localhost:${port} in your browser`));

	// Start the cron scheduler
	startScheduler(schedulerOpts).catch((err) => console.error(dim(`[scheduler] Init error: ${err.message}`)));

	return async () => {
		// Stop scheduled jobs
		stopScheduler();
		// Stop Telegram polling
		stopTelegramPolling();
		// Gracefully close WebSocket connections to trigger close handlers (journal, mood, etc.)
		for (const client of wss.clients) {
			client.close(1000, "Server shutting down");
		}
		// Wait for close handlers to fire, then await their async work (journal writes, etc.)
		await new Promise((r) => setTimeout(r, 200));
		if (pendingShutdownWork.length > 0) {
			console.log(dim("[voice] Waiting for journal & mood saves..."));
			await Promise.allSettled(pendingShutdownWork);
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
