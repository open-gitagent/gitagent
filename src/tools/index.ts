import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { SandboxContext } from "../sandbox.js";
import type { MemoryLayerDef } from "../plugin-types.js";
import type { CostTracker } from "../cost-tracker.js";
import { createCliTool } from "./cli.js";
import { createReadTool } from "./read.js";
import { createWriteTool } from "./write.js";
import { createEditTool } from "./edit.js";
import { createMemoryTool } from "./memory.js";
import { createTaskTrackerTool } from "./task-tracker.js";
import { createSkillLearnerTool } from "./skill-learner.js";
import { createCapturePhotoTool } from "./capture-photo.js";
import { createSandboxCliTool } from "./sandbox-cli.js";
import { createSandboxReadTool } from "./sandbox-read.js";
import { createSandboxWriteTool } from "./sandbox-write.js";
import { createSandboxEditTool } from "./sandbox-edit.js";
import { createSandboxMemoryTool } from "./sandbox-memory.js";
import { DocStore } from "./doc-store.js";

export interface BuiltinToolsConfig {
	dir: string;
	timeout?: number;
	sandbox?: SandboxContext;
	gitagentDir?: string;
	pluginMemoryLayers?: MemoryLayerDef[];
	costTracker?: CostTracker;
	docStore?: DocStore;
}

/**
 * Create the built-in tools (cli, read, write, memory, task_tracker, skill_learner).
 * If a SandboxContext is provided, returns sandbox-backed tools;
 * otherwise returns the standard local tools.
 */
export function createBuiltinTools(config: BuiltinToolsConfig): AgentTool<any>[] {
	if (config.sandbox) {
		return [
			createSandboxCliTool(config.sandbox, config.timeout),
			createSandboxReadTool(config.sandbox),
			createSandboxWriteTool(config.sandbox),
			createSandboxEditTool(config.sandbox),
			createSandboxMemoryTool(config.sandbox),
		];
	}

	const tools: AgentTool<any>[] = [
		createCliTool(config.dir, config.timeout),
		createReadTool(config.dir, config.costTracker, config.docStore),
		createWriteTool(config.dir),
		createEditTool(config.dir),
		createMemoryTool(config.dir, config.pluginMemoryLayers),
		createCapturePhotoTool(config.dir),
	];

	// CCR retrieval tool — only registered when a DocStore is active
	if (config.docStore) {
		const docStore = config.docStore;
		tools.push({
			name: "read_doc_section",
			label: "read_doc_section",
			description:
				"Retrieve a specific section of a document that was compressed via CCR. " +
				"Use the section ID (e.g. s1, s3) shown in the document outline returned by read.",
			parameters: {
				type: "object",
				properties: {
					path: { type: "string", description: "Absolute or relative path to the document (same as passed to read)" },
					section_id: { type: "string", description: "Section ID from the outline (e.g. s1, s4)" },
				},
				required: ["path", "section_id"],
			},
			execute: async (_toolCallId: string, params: unknown) => {
				const { path, section_id } = params as { path: string; section_id: string };
				const chunk = docStore.getChunk(path, section_id) ?? docStore.getChunk(
					path.startsWith("/") ? path : `${config.dir}/${path}`.replace(/\\/g, "/"),
					section_id,
				);
				if (!chunk) {
					return {
						content: [{ type: "text", text: `[No section "${section_id}" found for "${path}". Run read first.]` }],
						details: undefined,
					};
				}
				return {
					content: [{ type: "text", text: `## ${chunk.title}\n\n${chunk.content}` }],
					details: undefined,
				};
			},
		} satisfies AgentTool<any>);
	}

	// Add learning tools if gitagentDir is available
	if (config.gitagentDir) {
		tools.push(createTaskTrackerTool(config.dir, config.gitagentDir));
		tools.push(createSkillLearnerTool(config.dir, config.gitagentDir));
	}

	return tools;
}
