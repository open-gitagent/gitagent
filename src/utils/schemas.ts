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

	// Cross-field check: depends_on ids must reference declared step ids
	const data = parsed as any;
	if (Array.isArray(data.steps)) {
		const declared = new Set<string>();
		for (const step of data.steps) {
			if (step && typeof step.id === "string") declared.add(step.id);
		}
		data.steps.forEach((step: any, i: number) => {
			if (step && Array.isArray(step.depends_on)) {
				for (const dep of step.depends_on) {
					if (typeof dep === "string" && !declared.has(dep)) {
						issues.push({ path: `steps[${i}].depends_on`, message: `references unknown step id "${dep}"` });
					}
				}
			}
		});
	}

	if (issues.length === 0) {
		return { valid: true, errors: [], data: parsed as WorkflowDef };
	}
	return {
		valid: false,
		errors: issues.map((i) => (i.path ? `${i.path}: ${i.message}` : i.message)),
	};
}
