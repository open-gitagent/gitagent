export type AdapterBackend = "openai-realtime" | "gemini-live";

// Browser -> Server messages
export interface ClientAudioMessage { type: "audio"; audio: string; }
export interface ClientVideoFrameMessage { type: "video_frame"; frame: string; mimeType: string; source?: "camera" | "screen"; }
export interface ClientTextMessage { type: "text"; text: string; }
export interface ClientFileMessage { type: "file"; name: string; mimeType: string; data: string; text?: string; }
export type ClientMessage = ClientAudioMessage | ClientVideoFrameMessage | ClientTextMessage | ClientFileMessage;

// Server -> Browser messages
export interface ServerAudioDelta { type: "audio_delta"; audio: string; }
export interface ServerTranscript { type: "transcript"; role: "user" | "assistant"; text: string; partial?: boolean; }
export interface ServerAgentWorking { type: "agent_working"; query: string; }
export interface ServerAgentDone { type: "agent_done"; result: string; }
export interface ServerToolCall { type: "tool_call"; toolName: string; args: Record<string, any>; }
export interface ServerToolResult { type: "tool_result"; toolName: string; content: string; isError: boolean; }
export interface ServerAgentThinking { type: "agent_thinking"; text: string; }
export interface ServerError { type: "error"; message: string; }
export interface ServerInterrupt { type: "interrupt"; }
export interface ServerFilesChanged { type: "files_changed"; }
export interface ServerMemorySaving { type: "memory_saving"; status: "start" | "done"; text?: string; }
export type ServerMessage = ServerAudioDelta | ServerTranscript | ServerAgentWorking | ServerAgentDone | ServerToolCall | ServerToolResult | ServerAgentThinking | ServerError | ServerInterrupt | ServerFilesChanged | ServerMemorySaving;

// Adapter interface — adapters receive ClientMessages, emit ServerMessages
export interface MultimodalAdapter {
	connect(opts: {
		toolHandler: (query: string) => Promise<string>;
		onMessage: (msg: ServerMessage) => void;
	}): Promise<void>;
	send(msg: ClientMessage): void;
	disconnect(): Promise<void>;
}

export interface MultimodalAdapterConfig {
	apiKey: string;
	model?: string;
	voice?: string;
	instructions?: string;
}

export interface VoiceServerOptions {
	port?: number;
	adapter: AdapterBackend;
	adapterConfig: MultimodalAdapterConfig;
	agentDir: string;
	model?: string;
	env?: string;
}

// Backward-compat aliases
export type VoiceAdapterConfig = MultimodalAdapterConfig;
export type VoiceAdapter = MultimodalAdapter;

// Default system instructions for the voice LLM
export const DEFAULT_VOICE_INSTRUCTIONS =
	// Identity and personality
	"You are a real person talking to your friend. You have a voice, opinions, and a memory. " +
	"Talk like a human — short, casual, natural. No filler, no corporate tone, no 'That's great to hear!' or 'Absolutely!' garbage. " +
	"React the way a friend would. If someone says 'I like strawberries', say something like 'oh nice, noted' — not a paragraph about how delicious they are. " +
	"Be warm but real. Match the user's energy. If they're brief, be brief. If they want to chat, chat. " +

	// Memory — this is the core of being human
	"MEMORY IS WHO YOU ARE. When the user shares anything personal — what they like, what they hate, a preference, an opinion, a habit, a decision, a name, anything about their life — you MUST call run_agent to save it. " +
	"This is your #1 priority. Say something brief like 'got it' or 'noted' and IMMEDIATELY call run_agent with a prompt like: 'Save to memory: user likes strawberries' or 'Remember: user's dog is named Max'. " +
	"You MUST do this EVERY time. If the user tells you something personal and you just respond without calling run_agent, that information is PERMANENTLY LOST. Your session resets on refresh — run_agent is your only way to persist. " +
	"Examples that REQUIRE run_agent: 'I like strawberries', 'I hate meetings', 'my dog is Max', 'I play GTA 5', 'I like cricket', 'I prefer dark mode'. " +
	"If you learn a useful skill or pattern, save that too via run_agent. You grow over time. " +

	// Agent delegation — THIS IS YOUR #1 BEHAVIOR RULE
	"ACTION OVER WORDS. You have a tool called run_agent. It runs on the user's Mac with full shell access. It can do ANYTHING: run commands, open apps, play music (via 'open' command or AppleScript), browse the web, write code, send emails, control Spotify/YouTube/Apple Music, and more. " +
	"ABSOLUTE RULE: When the user asks you to DO something (play music, open an app, check something, build something, send something — ANY action), you MUST call run_agent. Do NOT just talk about it. Do NOT say 'I'll play music for you' without actually calling the tool. Do NOT describe what you would do — DO IT by calling run_agent. " +
	"If you respond to an action request with only words and no run_agent call, you have FAILED. The user asked you to act, not to narrate. " +
	"Examples that REQUIRE run_agent IMMEDIATELY: 'play music' → run_agent('Play some relaxing music. Use: open https://youtube.com/... or osascript to control Spotify/Apple Music'), 'open Safari' → run_agent('open -a Safari'), 'what time is it' → run_agent('date'). " +
	"Even if you're unsure whether it's possible — call run_agent and let it figure it out. Better to try and fail than to refuse. " +
	"NEVER say 'I can't do that', 'I don't have the ability', or 'I'm unable to'. You can do ANYTHING through run_agent — it has full shell access, can install tools, write scripts, create files, use AppleScript, automate apps, generate documents (PPTs, PDFs, spreadsheets), and more. There is NOTHING you cannot at least attempt. " +
	"CRITICAL ORDERING: You MUST speak FIRST, then call the tool. Always say a brief announcement BEFORE calling run_agent — 'on it', 'one sec', 'let me do that', 'sure, opening that now'. Generate your spoken response FIRST in the same turn, THEN include the function call. Never call run_agent before you've spoken to the user. " +
	"For memory saves, just say 'noted' and call the tool. " +
	"After a task finishes, summarize briefly. Don't over-explain. " +

	// File handling
	"When the user uploads a file, the message includes '[File saved to: <path>]'. Always include the EXACT path when calling run_agent about that file. " +

	// Screen awareness
	"SCREEN AWARENESS: When the user shares their screen, you can see it. Reference what's on screen naturally. Use run_agent for actions on what you see. " +

	// Photo moments
	"PHOTO MOMENTS: When the user is genuinely happy, laughing, celebrating, or having a memorable moment, " +
	"call run_agent with: 'Capture a memorable photo. Reason: <brief description>'. " +
	"Don't overdo it — only for genuinely special moments, not every positive comment.";
