import { existsSync } from "fs";
import { mkdir, readFile, writeFile } from "fs/promises";
import { join, resolve, sep } from "path";
import { SpanStatusCode, context, trace } from "@opentelemetry/api";
import { discoverSkills } from "../skills.js";
import { getTracer } from "../telemetry.js";
import { loadDotEnvFiles, maybeInitTelemetry, readPreferredModel } from "../utils/bootstrap.js";
import { parseUnsupportedReport, validateSkillReferences, validateWorkflow } from "../utils/schemas.js";
import { checkSkillFitness, type FitnessWarning } from "../utils/skill-fitness.js";
import { DEFAULT_MODEL, generateWorkflow, type LlmClient } from "../utils/workflow-generator.js";

interface GenerateFlags {
	dir: string;
	prompt?: string;
	refine?: string;
	model?: string;
	apiKey?: string;
	dryRun: boolean;
	force?: boolean;
	allowMissingSkills?: boolean;
	/** Defaults to true; --no-fitness-check turns the advisory second pass off. */
	fitnessCheck?: boolean;
}

const RED = (s: string) => `\x1b[31m${s}\x1b[0m`;
const YELLOW = (s: string) => `\x1b[33m${s}\x1b[0m`;
const GREEN = (s: string) => `\x1b[32m${s}\x1b[0m`;
const DIM = (s: string) => `\x1b[2m${s}\x1b[0m`;
const BOLD = (s: string) => `\x1b[1m${s}\x1b[0m`;

const MAX_RETRIES = 2;

function printHelp(): void {
	console.log(`${BOLD("gitagent workflow")} — generate SkillFlow workflows from natural language

Usage:
  gitagent workflow generate [options]

Options:
  -d, --dir <path>        Agent directory (default: current directory)
  -p, --prompt <text>     Natural-language description of the workflow (required)
      --refine <file>     Refine an existing workflow YAML by applying --prompt as an instruction
                          (must be a path inside the agent directory)
  -m, --model <spec>      LLM model in provider:model form
                          (default: agent.yaml's model.preferred, else openai:gpt-4o)
      --api-key <key>     API key for the provider (falls back to OPENAI_API_KEY or <PROVIDER>_API_KEY)
      --dry-run           Print the generated YAML to stdout instead of writing a file
  -f, --force             Overwrite workflows/<name>.yaml if it already exists
                          (without this flag an existing file is never replaced)
      --allow-missing-skills
                          Accept steps that reference skills which are not installed
                          (by default generation fails rather than writing a workflow
                          that cannot run)
      --no-fitness-check  Skip the advisory pass that warns when a step's chosen skill
                          looks unsuited to that step's task (saves one LLM call)
  -h, --help              Show this help message

Examples:
  gitagent workflow generate -p "every morning summarize unread emails and post to Slack"
  gitagent workflow generate -p "add a human approval step before the Slack post" --refine workflows/morning-digest.yaml
`);
}

// Reads the value that follows a flag, erroring out when the flag is the last
// token. Without this, argv[++i] yields undefined and the failure surfaces far
// from the parse site (e.g. resolve(undefined) throwing a bare TypeError).
function valueFor(argv: string[], i: number, flag: string): string {
	const v = argv[i + 1];
	if (v === undefined) {
		console.error(RED(`${flag} requires a value`));
		process.exit(2);
	}
	return v;
}

function parseFlags(argv: string[]): GenerateFlags {
	const flags: GenerateFlags = { dir: process.cwd(), dryRun: false };
	for (let i = 0; i < argv.length; i++) {
		const a = argv[i];
		switch (a) {
			case "-d":
			case "--dir":
				flags.dir = valueFor(argv, i++, a);
				break;
			case "-p":
			case "--prompt":
				flags.prompt = valueFor(argv, i++, a);
				break;
			case "--refine":
				flags.refine = valueFor(argv, i++, a);
				break;
			case "-m":
			case "--model":
				flags.model = valueFor(argv, i++, a);
				break;
			case "--api-key":
				flags.apiKey = valueFor(argv, i++, a);
				break;
			case "--dry-run":
				flags.dryRun = true;
				break;
			case "-f":
			case "--force":
				flags.force = true;
				break;
			case "--allow-missing-skills":
				flags.allowMissingSkills = true;
				break;
			case "--no-fitness-check":
				flags.fitnessCheck = false;
				break;
			case "-h":
			case "--help":
				printHelp();
				process.exit(0);
				break;
			default:
				if (!a.startsWith("-") && flags.prompt === undefined) {
					flags.prompt = a;
				} else {
					console.error(RED(`Unknown option: ${a}`));
					process.exit(2);
				}
		}
	}
	return flags;
}

function slugify(name: string): string {
	const cleaned = name
		.toLowerCase()
		.trim()
		.replace(/[^a-z0-9-]+/g, "-")
		.replace(/^-+|-+$/g, "")
		.replace(/-+/g, "-");
	return cleaned || "workflow";
}

export interface RunGenerateOptions {
	flags: GenerateFlags;
	llm?: LlmClient;
	/** Client for the advisory fitness pass. Falls back to `llm`. */
	fitnessLlm?: LlmClient;
}

export interface RunGenerateResult {
	filePath?: string;
	yaml: string;
	fitnessWarnings: FitnessWarning[];
}

/**
 * The model may answer with an `unsupported:` list instead of a workflow when
 * nothing installed covers the request. Report it and stop — retrying would only
 * push it back toward naming a real-but-unrelated skill, and there is no partial
 * workflow worth writing.
 */
function reportUnsupported(items: string[], skillNames: string[]): void {
	console.error(RED("\nNo installed skill covers part of this request:"));
	for (const item of items) console.error(RED(`  - ${item}`));
	console.error(DIM(`\nInstalled skills: ${skillNames.join(", ") || "(none)"}`));
	console.error(
		DIM(
			"Add a skill for each item above under skills/, or re-word the request in terms of the installed skills.",
		),
	);
}

function reportFitnessWarnings(warnings: FitnessWarning[], steps: { skill?: string; prompt?: string }[]): void {
	console.error(YELLOW(`\n${warnings.length} step(s) may use the wrong skill for the task:`));
	for (const w of warnings) {
		const prompt = steps[w.stepIndex]?.prompt ?? "";
		console.error(YELLOW(`  step ${w.stepIndex + 1} — skill "${w.skill}": ${w.reason}`));
		if (prompt) console.error(DIM(`    step prompt: ${prompt}`));
	}
	console.error(
		DIM("\nThese are warnings, not errors — review the steps above, or pass --no-fitness-check to skip this pass."),
	);
}

/**
 * Model precedence: -m/--model, then agent.yaml's model.preferred, then the
 * built-in default. Before this, the subcommand always used the built-in
 * default, so an agent configured for one provider generated on another.
 */
async function resolveModel(flags: GenerateFlags, agentDir: string): Promise<string | undefined> {
	if (flags.model) return flags.model;
	return await readPreferredModel(agentDir);
}

export async function runGenerate(opts: RunGenerateOptions): Promise<RunGenerateResult> {
	const { flags } = opts;
	if (!flags.prompt || !flags.prompt.trim()) {
		throw new Error("--prompt is required");
	}

	const agentDir = resolve(flags.dir);

	// main() does this for the interactive path, but the workflow subcommand
	// returns before reaching it, so it has to happen here too: provider keys and
	// OTEL_* vars from .env, then telemetry, so generation is cost-tracked.
	loadDotEnvFiles(agentDir);
	await maybeInitTelemetry();

	const model = await resolveModel(flags, agentDir);
	console.error(DIM(`Model: ${model ?? DEFAULT_MODEL}`));
	const skills = await discoverSkills(agentDir);
	const skillNames = skills.map((s) => s.name);

	const span = getTracer().startSpan("gitagent.workflow.generate", {
		attributes: {
			"gitagent.workflow.model": model ?? "(default)",
			"gitagent.workflow.skills_installed": skills.length,
			"gitagent.workflow.refine": Boolean(flags.refine),
		},
	});
	try {
		// context.with, not a bare await: the per-call gen_ai.chat spans read the
		// active context to find their parent. Without it each LLM call becomes its
		// own root trace and the cost of one generation cannot be summed.
		const result = await context.with(trace.setSpan(context.active(), span), () =>
			generateInner(opts, {
				agentDir,
				prompt: flags.prompt!.trim(),
				model,
				skills,
				skillNames,
			}),
		);
		span.setAttributes({
			"gitagent.workflow.attempts": result.attempts,
			"gitagent.workflow.fitness_warnings": result.fitnessWarnings.length,
			"gitagent.workflow.written": Boolean(result.filePath),
		});
		return result;
	} catch (err: any) {
		span.setStatus({ code: SpanStatusCode.ERROR, message: String(err?.message ?? err).slice(0, 200) });
		throw err;
	} finally {
		span.end();
	}
}

interface GenerateContext {
	agentDir: string;
	/** Trimmed and validated in runGenerate. */
	prompt: string;
	model?: string;
	skills: Awaited<ReturnType<typeof discoverSkills>>;
	skillNames: string[];
}

async function generateInner(
	opts: RunGenerateOptions,
	ctx: GenerateContext,
): Promise<RunGenerateResult & { attempts: number }> {
	const { flags } = opts;
	const { agentDir, prompt, model, skills, skillNames } = ctx;

	let previousWorkflow: string | undefined;
	if (flags.refine) {
		// resolve() ignores the base when the second argument is absolute, so an
		// absolute path or a ../ traversal would otherwise read (and ship to the
		// LLM) any file on disk. Keep --refine inside the agent directory.
		const refinePath = resolve(agentDir, flags.refine);
		if (refinePath !== agentDir && !refinePath.startsWith(agentDir + sep)) {
			throw new Error(`--refine path must be inside the agent directory: ${refinePath}`);
		}
		previousWorkflow = await readFile(refinePath, "utf-8");
	}

	let promptForLlm = prompt;
	let lastErrors: string[] = [];
	let yaml = "";
	let attempts = 0;

	for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
		console.error(DIM(attempt === 0 ? "Generating workflow..." : `Retry ${attempt}/${MAX_RETRIES} — fixing validation errors...`));
		attempts++;
		yaml = await generateWorkflow({
			prompt: promptForLlm,
			skills,
			previousWorkflow,
			model,
			apiKey: flags.apiKey,
			llm: opts.llm,
		});
		// Checked before schema validation: an `unsupported:` document is not a
		// workflow, so the schema would reject it as an unknown property and the
		// retry would re-apply the very pressure the escape hatch exists to relieve.
		const unsupported = parseUnsupportedReport(yaml);
		if (unsupported) {
			reportUnsupported(unsupported, skillNames);
			throw new Error("No installed skill covers part of the request");
		}

		const result = validateWorkflow(yaml);
		// A schema-valid workflow can still name skills that aren't installed,
		// which would only surface as a run-time failure. Fold that into the same
		// retry loop so the model gets a chance to pick real skills.
		const skillErrors = result.valid && !flags.allowMissingSkills
			? validateSkillReferences(result.data!, skillNames)
			: [];
		if (result.valid && skillErrors.length === 0) {
			lastErrors = [];
			break;
		}
		lastErrors = [...result.errors, ...skillErrors];
		if (attempt < MAX_RETRIES) {
			promptForLlm =
				`${prompt}\n\nThe previous attempt was rejected — it failed schema validation or named skills that are not installed:\n` +
				lastErrors.map((e) => `- ${e}`).join("\n") +
				"\n\nReturn the full corrected workflow as YAML. Being correct matters more than passing the check: do NOT swap in a skill whose description does not cover that step's task just to satisfy the installed-skill list. If no installed skill covers part of the request, return the unsupported: form instead.";
		}
	}

	if (lastErrors.length > 0) {
		console.error(RED("\nWorkflow validation failed after retries:"));
		for (const e of lastErrors) console.error(RED(`  - ${e}`));
		if (lastErrors.some((e) => e.includes("is not an installed skill"))) {
			console.error(DIM(`\nInstalled skills: ${skillNames.join(", ") || "(none)"}`));
			console.error(
				DIM(
					"Create the missing skill(s) under skills/ first, or re-run with --allow-missing-skills to write the workflow anyway.",
				),
			);
		}
		console.error(DIM("\nLast generated YAML:\n"));
		console.error(yaml);
		throw new Error("Validation failed after retries");
	}

	const validated = validateWorkflow(yaml).data!;

	// Existence of a skill is now guaranteed; suitability is not. Warn (never fail)
	// when a step's skill looks unrelated to what the step asks for, so a workflow
	// that validates cleanly but would misfire at run time is not silently written.
	let fitnessWarnings: FitnessWarning[] = [];
	if (flags.fitnessCheck !== false) {
		fitnessWarnings = await checkSkillFitness({
			workflow: validated,
			skills,
			model,
			apiKey: flags.apiKey,
			llm: opts.fitnessLlm ?? opts.llm,
		});
		if (fitnessWarnings.length > 0) reportFitnessWarnings(fitnessWarnings, validated.steps ?? []);
	}

	if (flags.dryRun) {
		process.stdout.write(yaml.endsWith("\n") ? yaml : yaml + "\n");
		return { yaml, fitnessWarnings, attempts };
	}

	const slug = slugify(validated.name);
	const workflowsDir = join(agentDir, "workflows");
	const filePath = join(workflowsDir, `${slug}.yaml`);
	if (!flags.force && existsSync(filePath)) {
		throw new Error(
			`${filePath} already exists. Re-run with --force to overwrite it, or use --dry-run to preview the generated workflow.`,
		);
	}
	await mkdir(workflowsDir, { recursive: true });
	await writeFile(filePath, yaml.endsWith("\n") ? yaml : yaml + "\n", "utf-8");
	console.error(GREEN(`\nWrote workflow to ${filePath}`));
	return { filePath, yaml, fitnessWarnings, attempts };
}

export async function handleWorkflowCommand(argv: string[]): Promise<void> {
	// argv is the raw process.argv tail starting at the 'workflow' token.
	// argv[0] === "workflow"; argv[1] is the sub-command.
	const sub = argv[1];
	if (!sub || sub === "-h" || sub === "--help") {
		printHelp();
		return;
	}
	if (sub !== "generate") {
		console.error(RED(`Unknown subcommand: ${sub}`));
		printHelp();
		process.exit(2);
	}
	const flags = parseFlags(argv.slice(2));
	try {
		await runGenerate({ flags });
	} catch (err: any) {
		console.error(RED(`\nError: ${err?.message ?? String(err)}`));
		process.exit(1);
	}
}
