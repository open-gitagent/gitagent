export type AssertionCategory = "format" | "content" | "quality" | "constraint" | "behavior" | "tone";
export type BriefStatus = "draft" | "approved" | "archived";
export type ComplexityLevel = "low" | "medium" | "high";
export type IssueLevel = "critical" | "warning" | "suggestion";

export interface BriefAssertion {
	id: number;
	category: AssertionCategory;
	assertion: string;
	why: string;
	test: string;
}

export interface BriefRubric {
	craft: string;
	originality: string;
	tone: string;
	completeness: string;
}

export interface BriefDraft {
	task_summary: string;
	ambiguities: string[];
	assertions: BriefAssertion[];
	rubric: BriefRubric;
	constraints_applied: string[];
	estimated_complexity: ComplexityLevel;
	recommended_max_turns: number;
}

export interface BriefIssue {
	level: IssueLevel;
	assertion_id?: number | null;
	field?: "assertions" | "rubric" | "ambiguities" | "overall";
	issue: string;
	fix: string;
}

export interface EvaluatorVerdict {
	approved: boolean;
	score: number;
	issues: BriefIssue[];
	summary: string;
}

export interface Brief {
	id: string;
	task: string;
	agent: string;
	created_at: string;
	approved_at?: string;
	status: BriefStatus;
	version: number;
	planner_model: string;
	evaluator_model: string;
	negotiation_iterations: number;
	soul_hash: string;
	rules_hash: string;
	draft: BriefDraft;
	file_path: string;
}

export interface BriefOptions {
	briefPath?: string;
	autoBrief?: boolean;
	skipApproval?: boolean;
	regenerate?: boolean;
	plannerModel?: string;
	evaluatorModel?: string;
	allowBestEffort?: boolean;
}

export interface NegotiatorOptions {
	task: string;
	soul: string;
	rules: string;
	duties: string;
	model?: string;
	agentDir: string;
}

export interface NegotiationResult {
	draft: BriefDraft;
	verdict: EvaluatorVerdict;
	iterations: number;
	bestEffort: boolean;
	costs: import("../cost-tracker.js").SessionCosts;
}

export interface AssertionResult {
	assertion_id: number;
	category: AssertionCategory;
	assertion: string;
	passed: boolean;
	evidence: string;
	notes?: string;
}

export interface OutputVerdict {
	all_passed: boolean;
	passed_count: number;
	failed_count: number;
	results: AssertionResult[];
	summary: string;
}

export interface RunWithBriefOptions {
	prompt: string;
	dir?: string;
	model?: string;
	briefModel?: string;  // model used for Output Evaluator; falls back to model if not set
	brief: BriefOptions;
	maxRetries?: number;
	autoRetry?: boolean;
	showReport?: boolean;
	env?: string;
	hooks?: any;
	abortController?: AbortController;
	sessionId?: string;
}

export class BriefError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "BriefError";
	}
}

export class BriefGenerationError extends BriefError {
	constructor(message: string) {
		super(message);
		this.name = "BriefGenerationError";
	}
}
