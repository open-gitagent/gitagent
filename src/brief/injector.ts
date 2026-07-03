import type { Brief } from "./types.js";

export function buildBriefSuffix(brief: Brief): string {
	const draft = brief.draft;

	const assertionLines = draft.assertions.map(a =>
		`${a.id}. [${a.category}] ${a.assertion}\n   → Verify: ${a.test}`,
	).join("\n\n");

	const constraintsSection = draft.constraints_applied.length > 0
		? `\n**Active constraints from your rules:**\n${draft.constraints_applied.map(c => `- ${c}`).join("\n")}`
		: "";

	const ambiguitiesSection = draft.ambiguities.length > 0
		? `\n**Resolved ambiguities:**\n${draft.ambiguities.map(a => `- ${a}`).join("\n")}`
		: "";

	return `---
## Active Brief

You are executing against an approved brief. Your output MUST satisfy every assertion below.
Self-evaluate against each criterion before considering your response complete.

**Task:** ${brief.task}

**Success Criteria — all must pass:**

${assertionLines}

**Quality Standard:**
- Craft: ${draft.rubric.craft}
- Originality: ${draft.rubric.originality}
- Tone: ${draft.rubric.tone}
- Completeness: ${draft.rubric.completeness}
${constraintsSection}
${ambiguitiesSection}

Before finishing, verify each numbered assertion above against your output.
If any assertion fails, revise before responding.
---`.trim();
}
