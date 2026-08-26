import { readFileSync, existsSync } from "fs";
import { dirname, join, resolve } from "path";
import { fileURLToPath } from "url";
import yaml from "js-yaml";

export interface WorkflowStep {
	id?: string;
	skill: string;
	prompt: string;
	channel?: string;
	depends_on?: string[];
	requires_approval?: boolean;
}

export interface WorkflowDef {
	name: string;
	description: string;
	steps: WorkflowStep[];
}

export interface ValidationResult {
	valid: boolean;
	errors: string[];
	data?: WorkflowDef;
}

let cachedSchema: any = null;
let cachedSchemaText: string | null = null;

function resolveSchemaPath(): string {
	const here = dirname(fileURLToPath(import.meta.url));
	// Try candidates relative to this module's location.
	// 1. Running from src/utils/ in tests: ../../spec/schemas/workflow.schema.json
	// 2. Running from dist/utils/ after build: ../../spec/schemas/workflow.schema.json
	// 3. Running from dist/utils/ when spec/ is not packed: walk upward.
	const candidates = [
		resolve(here, "..", "..", "spec", "schemas", "workflow.schema.json"),
		resolve(here, "..", "..", "..", "spec", "schemas", "workflow.schema.json"),
	];
	for (const p of candidates) {
		if (existsSync(p)) return p;
	}
	// Fallback: walk up to 6 levels looking for the schema.
	let cur = here;
	for (let i = 0; i < 6; i++) {
		const guess = join(cur, "spec", "schemas", "workflow.schema.json");
		if (existsSync(guess)) return guess;
		const parent = dirname(cur);
		if (parent === cur) break;
		cur = parent;
	}
	throw new Error(`Could not locate spec/schemas/workflow.schema.json relative to ${here}`);
}

export function loadWorkflowSchema(): any {
	if (cachedSchema) return cachedSchema;
	const path = resolveSchemaPath();
	cachedSchemaText = readFileSync(path, "utf-8");
	cachedSchema = JSON.parse(cachedSchemaText);
	return cachedSchema;
}

export function getWorkflowSchemaText(): string {
	if (cachedSchemaText) return cachedSchemaText;
	loadWorkflowSchema();
	return cachedSchemaText!;
}

function typeOf(v: any): string {
	if (v === null) return "null";
	if (Array.isArray(v)) return "array";
	return typeof v;
}

function matchesType(v: any, expected: string | string[]): boolean {
	const types = Array.isArray(expected) ? expected : [expected];
	const actual = typeOf(v);
	return types.includes(actual) || (types.includes("integer") && actual === "number" && Number.isInteger(v));
}

interface Issue {
	path: string;
	message: string;
}

function validateAgainst(data: any, schema: any, path: string, root: any, issues: Issue[]): void {
	// Resolve $ref
	if (schema && typeof schema === "object" && schema.$ref) {
		const ref = schema.$ref as string;
		if (!ref.startsWith("#/")) {
			issues.push({ path, message: `unsupported $ref "${ref}" (only local refs are supported)` });
			return;
		}
		const segments = ref.slice(2).split("/");
		let resolved: any = root;
		for (const seg of segments) {
			resolved = resolved?.[seg];
		}
		if (!resolved) {
			issues.push({ path, message: `cannot resolve $ref "${ref}"` });
			return;
		}
		validateAgainst(data, resolved, path, root, issues);
		return;
	}

	if (!schema || typeof schema !== "object") return;

	if (schema.type && !matchesType(data, schema.type)) {
		issues.push({
			path,
			message: `expected type ${Array.isArray(schema.type) ? schema.type.join("|") : schema.type}, got ${typeOf(data)}`,
		});
		return;
	}

	if (typeOf(data) === "object") {
		const required: string[] = Array.isArray(schema.required) ? schema.required : [];
		for (const key of required) {
			if (!(key in data)) {
				issues.push({ path: path || "(root)", message: `missing required property "${key}"` });
			}
		}

		const props = schema.properties ?? {};
		const additionalAllowed = schema.additionalProperties !== false;
		for (const key of Object.keys(data)) {
			const childPath = path ? `${path}.${key}` : key;
			if (props[key]) {
				validateAgainst(data[key], props[key], childPath, root, issues);
			} else if (!additionalAllowed) {
				issues.push({ path: path || "(root)", message: `unknown property "${key}"` });
			}
		}
	} else if (typeOf(data) === "array") {
		if (schema.minItems != null && data.length < schema.minItems) {
			issues.push({ path: path || "(root)", message: `array must have at least ${schema.minItems} item(s), got ${data.length}` });
		}
		if (schema.items) {
			for (let i = 0; i < data.length; i++) {
				validateAgainst(data[i], schema.items, `${path}[${i}]`, root, issues);
			}
		}
		if (schema.uniqueItems === true) {
			const seen = new Set<string>();
			for (let i = 0; i < data.length; i++) {
				const key = JSON.stringify(data[i]);
				if (seen.has(key)) {
					issues.push({ path: `${path}[${i}]`, message: `duplicate item` });
				}
				seen.add(key);
			}
		}
	} else if (typeOf(data) === "string") {
		if (schema.minLength != null && data.length < schema.minLength) {
			issues.push({ path: path || "(root)", message: `string must be at least ${schema.minLength} character(s)` });
		}
		if (schema.pattern && !new RegExp(schema.pattern).test(data)) {
			issues.push({ path: path || "(root)", message: `value "${data}" does not match pattern ${schema.pattern}` });
		}
	}
}

/**
 * Depth-first search over the depends_on graph. Returns the offending path
 * (e.g. ["a", "b", "a"]) for the first cycle found, or null when the graph is
 * acyclic. Edges pointing at undeclared ids are ignored — those are already
 * reported by the unknown-id check.
 */
function findDependencyCycle(steps: any[]): string[] | null {
	const edges = new Map<string, string[]>();
	for (const step of steps) {
		if (!step || typeof step.id !== "string") continue;
		const deps = Array.isArray(step.depends_on) ? step.depends_on.filter((d: any) => typeof d === "string") : [];
		// Later duplicates of an id merge rather than overwrite, so no edge is lost.
		edges.set(step.id, [...(edges.get(step.id) ?? []), ...deps]);
	}

	const done = new Set<string>();
	const stack: string[] = [];
	const onStack = new Set<string>();

	function visit(id: string): string[] | null {
		if (done.has(id)) return null;
		if (onStack.has(id)) return [...stack.slice(stack.indexOf(id)), id];
		onStack.add(id);
		stack.push(id);
		for (const dep of edges.get(id) ?? []) {
			if (!edges.has(dep)) continue;
			const found = visit(dep);
			if (found) return found;
		}
		stack.pop();
		onStack.delete(id);
		done.add(id);
		return null;
	}

	for (const id of edges.keys()) {
		const found = visit(id);
		if (found) return found;
	}
	return null;
}

/**
 * Pseudo-skill for human-approval steps. It never exists under `skills/`, so it
 * has to be exempt from the installed-skill check below.
 */
export const APPROVAL_SKILL = "approval";

/**
 * Cross-check every step's `skill` against the skills actually installed in the
 * agent directory. The JSON Schema can only check shape, so without this a
 * plausible-looking workflow referencing skills that don't exist is written to
 * disk and fails at run time instead of at generation time.
 *
 * Returns validator-shaped error strings so callers can fold them into the same
 * retry loop as schema errors. An empty `installedSkills` list disables the
 * check — that is the same escape hatch the generator's system prompt takes,
 * where the model is told to invent sensible skill names.
 */
export function validateSkillReferences(workflow: WorkflowDef, installedSkills: string[]): string[] {
	if (installedSkills.length === 0) return [];
	const known = new Set([...installedSkills, APPROVAL_SKILL]);
	const errors: string[] = [];
	const steps = Array.isArray(workflow?.steps) ? workflow.steps : [];
	steps.forEach((step: any, i: number) => {
		const skill = typeof step?.skill === "string" ? step.skill.trim() : "";
		if (skill && !known.has(skill)) {
			errors.push(`steps[${i}].skill: "${skill}" is not an installed skill`);
		}
	});
	return errors;
}

/**
 * Detects the "nothing installed covers this" response the generator's system
 * prompt allows the model to return instead of a workflow:
 *
 *   unsupported:
 *     - fetch the current weather forecast
 *     - send a text message
 *
 * Without this escape hatch the installed-skill check pushes the model into
 * picking whichever real skill happens to validate, producing a workflow that
 * passes every check and then does the wrong thing at run time. Callers must
 * check this *before* validateWorkflow — an `unsupported` document is not a
 * workflow, so the schema would reject it as an unknown property and the
 * resulting retry would just re-apply that same pressure.
 *
 * Returns the non-empty trimmed items, or null when the document is not a
 * decline (including `unsupported: []`, which says nothing and is left to fail
 * schema validation normally).
 */
export function parseUnsupportedReport(yamlText: string): string[] | null {
	let parsed: unknown;
	try {
		parsed = yaml.load(yamlText);
	} catch {
		return null;
	}
	if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
	const raw = (parsed as any).unsupported;
	if (raw === undefined || raw === null) return null;
	const items = (Array.isArray(raw) ? raw : [raw])
		.map((v) => {
			if (typeof v === "string") return v.trim();
			if (typeof v === "number" || typeof v === "boolean") return String(v);
			return "";
		})
		.filter((v) => v.length > 0);
	return items.length > 0 ? items : null;
}

export function validateWorkflow(yamlText: string): ValidationResult {
	let parsed: unknown;
	try {
		parsed = yaml.load(yamlText);
	} catch (err: any) {
		return { valid: false, errors: [`YAML parse error: ${err?.message ?? String(err)}`] };
	}

	if (parsed === null || parsed === undefined) {
		return { valid: false, errors: ["workflow is empty"] };
	}

	if (typeof parsed !== "object" || Array.isArray(parsed)) {
		return { valid: false, errors: [`workflow must be an object, got ${typeOf(parsed)}`] };
	}

	const schema = loadWorkflowSchema();
	const issues: Issue[] = [];
	validateAgainst(parsed, schema, "", schema, issues);

	// Cross-field check: depends_on ids must reference a step declared earlier.
	// The set grows as steps are visited rather than being pre-built, so a forward
	// reference is rejected here instead of passing validation and failing later in
	// loadFlowDefinition — after the file is already on disk and out of the retry
	// loop. Steps execute in declaration order, so these are the same semantics.
	const data = parsed as any;
	if (Array.isArray(data.steps)) {
		const available = new Set<string>();
		data.steps.forEach((step: any, i: number) => {
			if (step && Array.isArray(step.depends_on)) {
				for (const dep of step.depends_on) {
					if (typeof dep === "string" && !available.has(dep)) {
						issues.push({
							path: `steps[${i}].depends_on`,
							message: `references unknown step id "${dep}" (must be declared in a preceding step)`,
						});
					}
				}
			}
			if (step && typeof step.id === "string") available.add(step.id);
		});

		// Cross-field check: the depends_on graph must be acyclic. A self-reference
		// or an A -> B -> A cycle would deadlock (or loop) at execution time.
		const cycle = findDependencyCycle(data.steps);
		if (cycle) {
			issues.push({ path: "steps", message: `depends_on cycle detected: ${cycle.join(" -> ")}` });
		}
	}

	if (issues.length === 0) {
		return { valid: true, errors: [], data: parsed as WorkflowDef };
	}
	return {
		valid: false,
		errors: issues.map((i) => (i.path ? `${i.path}: ${i.message}` : i.message)),
	};
}
