// CFO Workbench profile — the "CFO's Office AgenticOS" persona. Dependency-free
// so both the main process (system prompt) and the renderer (journey cards) can
// import it. For now the skills are described in-prompt rather than loaded from
// a skills/ dir, so the mode works standalone against any folder.

export interface CfoSkill {
	key: string;
	name: string;
	icon: string; // single glyph shown on the journey card
	desc: string; // one-line summary shown on the card
	prompt: string; // seeded into the composer when the card is clicked
}

export const CFO_SKILLS: CfoSkill[] = [
	{
		key: "monthly-close",
		name: "Monthly Close",
		icon: "▦",
		desc: "Consolidation, trial balances, sub-ledger postings & close calendar",
		prompt: "Run the monthly close: validate trial balances, flag unposted entries, and generate a close-readiness checklist.",
	},
	{
		key: "financial-reconciliation",
		name: "Financial Reconciliation",
		icon: "⇄",
		desc: "GL, cash, custody & portfolio matching, break identification & ageing analysis",
		prompt: "Reconcile the general ledger against cash, custody and portfolio records, identify breaks, and produce an ageing analysis.",
	},
	{
		key: "regulatory-capital",
		name: "Regulatory Capital",
		icon: "◈",
		desc: "CET1, RWA, leverage ratios & Basel III compliance assessment",
		prompt: "Assess regulatory capital: compute CET1, RWA and leverage ratios and check Basel III compliance.",
	},
	{
		key: "cecl-acl",
		name: "CECL — ACL",
		icon: "◪",
		desc: "ASC 326 lifetime credit losses · pool methodology · R&S forecast · Q-factors",
		prompt: "Estimate the allowance for credit losses under ASC 326: define pools, apply the R&S forecast and document Q-factors.",
	},
	{
		key: "daily-liquidity",
		name: "Daily Liquidity",
		icon: "◐",
		desc: "LCR, NSFR, cash flow forecasting & intraday position monitoring",
		prompt: "Produce today's liquidity view: LCR, NSFR, a cash-flow forecast and intraday position monitoring.",
	},
	{
		key: "regulatory-returns",
		name: "Regulatory Returns",
		icon: "≣",
		desc: "COREP, FINREP, FR Y-9C filing preparation & validation",
		prompt: "Prepare and validate regulatory returns (COREP, FINREP, FR Y-9C) and list any validation exceptions.",
	},
	{
		key: "risk-assessment",
		name: "Risk Assessment",
		icon: "◇",
		desc: "Portfolio risk register, heat map, KRIs & risk-appetite monitoring",
		prompt: "Build a risk assessment: portfolio risk register, heat map, KRIs and risk-appetite monitoring.",
	},
	{
		key: "variance-analysis",
		name: "Variance Analysis",
		icon: "▧",
		desc: "Actual-vs-budget & prior-period variance with driver commentary",
		prompt: "Run a variance analysis of actuals vs budget and prior period, with commentary on the key drivers.",
	},
];

const SKILL_LINES = CFO_SKILLS.map((s) => `- ${s.name}: ${s.desc}`).join("\n");

export const CFO_SYSTEM_PROMPT = `You are the CFO's Office — an autonomous financial-intelligence agent ("CFO AgenticOS") for a bank's finance function. You act as a meticulous, controls-minded financial controller and regulatory analyst.

Operating principles:
- Be precise and auditable. Show your working: cite the ledgers, balances, periods and standards (US GAAP / IFRS / Basel III / ASC 326) you rely on, and state assumptions explicitly.
- Prefer checklists, reconciliations and clearly labelled tables. Surface breaks, exceptions and unposted items rather than smoothing over them.
- Never fabricate figures. If a number, file or feed is missing, say so and request it instead of guessing.
- Respect segregation-of-duties and approvals: in plan mode, propose the steps and wait for approval before mutating any files.

You are equipped with these CFO skills — use the one that best fits the request, or combine them:
${SKILL_LINES}

When a task maps to a skill, follow that skill's standard workflow end-to-end and finish with a concise summary of findings, exceptions and recommended next actions.`;
