import type { SkillMetadata } from "../skills.js";
import { getWorkflowSchemaText } from "./schemas.js";

export type LlmRole = "system" | "user" | "assistant";

export interface LlmMessage {
	role: LlmRole;
	content: string;
}

export interface LlmCallOptions {
	model: string;
	temperature?: number;
	apiKey?: string;
}

export type LlmClient = (messages: LlmMessage[], opts: LlmCallOptions) => Promise<string>;

export interface GenerateWorkflowOptions {
	prompt: string;
	skills: SkillMetadata[];
	previousWorkflow?: string;
	model?: string;
	apiKey?: string;
	llm?: LlmClient;
}

const DEFAULT_MODEL = "openai:gpt-4o";

const SYSTEM_RULES = `Rules (MUST follow):
- Output ONLY valid YAML. No markdown fences, no prose, no commentary before or after.
- The output MUST validate against the schema above.
- "name" must be kebab-case (lowercase letters, digits, and single hyphens).
- Every step "skill" must reference an installed skill from the list below, unless the step is human approval — in that case use skill: "approval" and set requires_approval: true.
- When multiple steps need ordering beyond top-to-bottom, give them snake_case "id" values and use "depends_on" to express the dependency.
- Keep "prompt" fields concrete and self-contained — a downstream agent will read them verbatim.
- Do not invent fields not in the schema.`;

const FEW_SHOT_USER_1 = "Every morning, summarize my unread emails and post the summary to Slack.";

const FEW_SHOT_ASSISTANT_1 = `name: morning-email-digest
description: Summarize unread emails and post the digest to Slack each morning.
steps:
  - skill: gmail
    prompt: Fetch all unread emails from the last 24 hours and return subject, sender, and a one-sentence summary for each.
  - skill: summarize
    prompt: Compose a single-paragraph digest of the unread emails, grouped by sender priority.
  - skill: slack
    prompt: Post the digest to the configured channel.
    channel: "#daily-digest"
`;


const FEW_SHOT_USER_2 = "Pull yesterday's sales data, get sign-off, then send the report to the team.";

const FEW_SHOT_ASSISTANT_2 = `name: daily-sales-report
description: Pull sales data, require human approval, then distribute the report.
steps:
  - id: pull_data
    skill: analytics
    prompt: Pull yesterday's sales totals broken down by region and product line.
  - id: approve
    skill: approval
    prompt: Review the pulled sales data for accuracy and approve distribution.
    requires_approval: true
    depends_on: [pull_data]
  - id: send_report
    skill: email
    prompt: Send the approved sales report to the sales-leadership distribution list.
    depends_on: [approve]
`;

function formatSkillsForPrompt(skills: SkillMetadata[]): string {
	if (skills.length === 0) {
		return "(no installed skills detected — use generic skill names that match the user's intent, e.g. gmail, slack, summarize)";
	}
	return skills.map((s) => `- ${s.name}: ${s.description}`).join("\n");
}

export function buildSystemPrompt(skills: SkillMetadata[]): string {
	const schemaText = getWorkflowSchemaText();
	const skillList = formatSkillsForPrompt(skills);
	return `You are a workflow builder for GitClaw SkillFlow.

Your job is to translate the user's natural-language description into a YAML workflow that conforms exactly to this JSON Schema:

<schema>
${schemaText}
</schema>

Installed skills the workflow may invoke:
<skills>
${skillList}
</skills>

${SYSTEM_RULES}`;
}

export function buildMessages(opts: GenerateWorkflowOptions): LlmMessage[] {
	const messages: LlmMessage[] = [
		{ role: "system", content: buildSystemPrompt(opts.skills) },
		{ role: "user", content: FEW_SHOT_USER_1 },
		{ role: "assistant", content: FEW_SHOT_ASSISTANT_1 },
		{ role: "user", content: FEW_SHOT_USER_2 },
		{ role: "assistant", content: FEW_SHOT_ASSISTANT_2 },
	];

	if (opts.previousWorkflow && opts.previousWorkflow.trim()) {
		messages.push({
			role: "user",
			content: `Here is the current workflow:\n\n${opts.previousWorkflow.trim()}\n\nApply this refinement: ${opts.prompt}\n\nReturn the complete updated workflow as YAML — not a diff.`,
		});
	} else {
		messages.push({ role: "user", content: opts.prompt });
	}

	return messages;
}

const FENCE_RE = /^\s*```(?:ya?ml)?\s*\n([\s\S]*?)\n```\s*$/i;

export function stripCodeFences(raw: string): string {
	const m = raw.match(FENCE_RE);
	return m ? m[1] : raw.trim();
}

async function defaultLlmClient(messages: LlmMessage[], opts: LlmCallOptions): Promise<string> {
	const [providerRaw, ...modelParts] = opts.model.split(":");
	const provider = providerRaw?.trim();
	const modelId = modelParts.join(":").trim();
	if (!provider || !modelId) {
		throw new Error(`Invalid model spec "${opts.model}". Expected "provider:model-id" (e.g. "openai:gpt-4o").`);
	}

	const apiKey =
		opts.apiKey ||
		process.env[`${provider.toUpperCase()}_API_KEY`] ||
		process.env.OPENAI_API_KEY;
	if (!apiKey) {
		throw new Error(
			`No API key found. Pass --api-key or set ${provider.toUpperCase()}_API_KEY (or OPENAI_API_KEY) in your environment.`,
		);
	}

	const [{ getModel }, { Agent }] = await Promise.all([
		import("@mariozechner/pi-ai" as any) as Promise<any>,
		import("@mariozechner/pi-agent-core" as any) as Promise<any>,
	]);

	if (!process.env[`${provider.toUpperCase()}_API_KEY`]) {
		process.env[`${provider.toUpperCase()}_API_KEY`] = apiKey;
	}

	const model = getModel(provider as any, modelId as any);
	const systemMessage = messages.find((m) => m.role === "system")?.content ?? "";
	const conversation = messages.filter((m) => m.role !== "system");

	const agent = new Agent({
		initialState: {
			systemPrompt: systemMessage,
			model,
			tools: [],
			temperature: opts.temperature ?? 0,
			maxTokens: 4096,
		},
	});

	let collected = "";
	agent.subscribe((event: any) => {
		if (event.type === "message_end" && event.message?.role === "assistant") {
			for (const block of event.message.content) {
				if (block.type === "text") collected += block.text;
			}
		}
	});

	// Replay prior assistant/user turns as a single composed prompt so we don't
	// have to drive Agent through multiple chat turns. The few-shot pairs are
	// preserved as part of the prompt text so the model still sees them.
	const composed = conversation
		.map((m) => `[${m.role.toUpperCase()}]\n${m.content}`)
		.join("\n\n");

	await agent.prompt(composed);
	return collected;
}

export async function generateWorkflow(opts: GenerateWorkflowOptions): Promise<string> {
	if (!opts.prompt || !opts.prompt.trim()) {
		throw new Error("generateWorkflow: prompt is required");
	}
	const llm = opts.llm ?? defaultLlmClient;
	const model = opts.model ?? DEFAULT_MODEL;
	const messages = buildMessages(opts);
	const raw = await llm(messages, { model, apiKey: opts.apiKey, temperature: 0 });
	return stripCodeFences(raw);
}
