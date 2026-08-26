// Unit tests for src/utils/schemas.ts — the SkillFlow workflow validator.

import test from "node:test";
import assert from "node:assert/strict";

import { validateWorkflow, loadWorkflowSchema, validateSkillReferences } from "../src/utils/schemas.ts";

const VALID_YAML = `name: morning-digest
description: Summarize unread emails and post to Slack each morning.
steps:
  - skill: gmail
    prompt: Fetch unread emails from the last 24h.
  - skill: summarize
    prompt: Compose a digest grouped by sender priority.
  - skill: slack
    prompt: Post the digest to the team channel.
    channel: "#daily-digest"
`;

test("loadWorkflowSchema returns the parsed schema with required top-level keys", () => {
	const schema = loadWorkflowSchema();
	assert.equal(typeof schema, "object");
	assert.deepEqual(schema.required, ["name", "description", "steps"]);
	assert.equal(schema.definitions.step.required.includes("skill"), true);
	assert.equal(schema.definitions.step.required.includes("prompt"), true);
});

test("validateWorkflow accepts a well-formed workflow", () => {
	const r = validateWorkflow(VALID_YAML);
	assert.equal(r.valid, true);
	assert.deepEqual(r.errors, []);
	assert.equal(r.data?.name, "morning-digest");
	assert.equal(r.data?.steps.length, 3);
});

test("validateWorkflow rejects missing name", () => {
	const yaml = `description: foo
steps:
  - skill: gmail
    prompt: do thing
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(r.errors.some((e) => e.includes('missing required property "name"')), `errors: ${JSON.stringify(r.errors)}`);
});

test("validateWorkflow rejects non-kebab-case name", () => {
	const yaml = `name: MyWorkflow
description: foo
steps:
  - skill: gmail
    prompt: do thing
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(r.errors.some((e) => e.includes("pattern")), `errors: ${JSON.stringify(r.errors)}`);
});

test("validateWorkflow rejects empty steps array", () => {
	const yaml = `name: empty-flow
description: nothing
steps: []
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(r.errors.some((e) => e.includes("at least 1")), `errors: ${JSON.stringify(r.errors)}`);
});

test("validateWorkflow rejects a step missing required prompt", () => {
	const yaml = `name: bad-step
description: missing prompt
steps:
  - skill: gmail
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(
		r.errors.some((e) => e.includes('missing required property "prompt"')),
		`errors: ${JSON.stringify(r.errors)}`,
	);
});

test("validateWorkflow rejects unknown step property", () => {
	const yaml = `name: extra-prop
description: bad
steps:
  - skill: gmail
    prompt: do thing
    nonsense: true
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(
		r.errors.some((e) => e.includes('unknown property "nonsense"')),
		`errors: ${JSON.stringify(r.errors)}`,
	);
});

test("validateWorkflow flags depends_on referencing a missing id", () => {
	const yaml = `name: bad-deps
description: dangling dep
steps:
  - id: a
    skill: gmail
    prompt: fetch
  - skill: slack
    prompt: post
    depends_on: [does_not_exist]
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(
		r.errors.some((e) => e.includes('references unknown step id "does_not_exist"')),
		`errors: ${JSON.stringify(r.errors)}`,
	);
});

test("validateWorkflow flags a self-referencing depends_on", () => {
	const yaml = `name: self-cycle
description: step depends on itself
steps:
  - id: a
    skill: gmail
    prompt: fetch
    depends_on: [a]
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(r.errors.some((e) => e.includes("cycle")), `errors: ${JSON.stringify(r.errors)}`);
});

test("validateWorkflow flags an A -> B -> A depends_on cycle", () => {
	const yaml = `name: mutual-cycle
description: two steps depend on each other
steps:
  - id: a
    skill: gmail
    prompt: fetch
    depends_on: [b]
  - id: b
    skill: slack
    prompt: post
    depends_on: [a]
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(
		r.errors.some((e) => e.includes("depends_on cycle detected")),
		`errors: ${JSON.stringify(r.errors)}`,
	);
});

test("validateWorkflow rejects a forward depends_on reference", () => {
	// Steps run in declaration order, so depending on a later step can never be
	// satisfied. Catching it here keeps validate in step with loadFlowDefinition
	// and gives the retry loop a chance to fix it before anything is written.
	const yaml = `name: forward-ref
description: first step depends on the second
steps:
  - skill: gmail
    prompt: fetch
    depends_on: [post]
  - id: post
    skill: slack
    prompt: post
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(
		r.errors.some((e) => e.includes('references unknown step id "post"') && e.includes("preceding step")),
		`errors: ${JSON.stringify(r.errors)}`,
	);
});

test("validateWorkflow accepts a diamond-shaped depends_on graph", () => {
	const yaml = `name: diamond-flow
description: fan out then fan in
steps:
  - id: root
    skill: gmail
    prompt: fetch
  - id: left
    skill: summarize
    prompt: summarize inbox
    depends_on: [root]
  - id: right
    skill: summarize
    prompt: summarize archive
    depends_on: [root]
  - id: post
    skill: slack
    prompt: post both summaries
    depends_on: [left, right]
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, true, `errors: ${JSON.stringify(r.errors)}`);
});

test("validateWorkflow accepts approval step with requires_approval", () => {
	const yaml = `name: approval-flow
description: needs sign-off
steps:
  - id: pull
    skill: analytics
    prompt: Pull data.
  - id: approve
    skill: approval
    prompt: Approve distribution.
    requires_approval: true
    depends_on: [pull]
  - skill: email
    prompt: Send report.
    depends_on: [approve]
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, true, `errors: ${JSON.stringify(r.errors)}`);
});

test("validateWorkflow surfaces YAML parse errors", () => {
	const yaml = `name: bad
description: : :
steps:
  - skill: [unterminated
`;
	const r = validateWorkflow(yaml);
	assert.equal(r.valid, false);
	assert.ok(r.errors[0].startsWith("YAML parse error"), `errors: ${JSON.stringify(r.errors)}`);
});

test("validateWorkflow rejects empty document", () => {
	const r = validateWorkflow("");
	assert.equal(r.valid, false);
	assert.ok(r.errors[0].includes("empty"));
});

// ── validateSkillReferences ────────────────────────────────────────────

const UNKNOWN_SKILLS_YAML = `name: morning-weather-summary
description: Check the weather each morning and send a text summary.
steps:
  - id: fetch_weather
    skill: weather
    prompt: Fetch today's forecast.
  - id: send_summary
    skill: sms
    prompt: Text a one-sentence summary.
    depends_on: [fetch_weather]
`;

test("validateSkillReferences reports every step naming an uninstalled skill", () => {
	const data = validateWorkflow(UNKNOWN_SKILLS_YAML).data!;
	const errors = validateSkillReferences(data, ["gmail", "slack", "summarize"]);
	assert.deepEqual(errors, [
		'steps[0].skill: "weather" is not an installed skill',
		'steps[1].skill: "sms" is not an installed skill',
	]);
});

test("validateSkillReferences passes when every skill is installed", () => {
	const data = validateWorkflow(VALID_YAML).data!;
	assert.deepEqual(validateSkillReferences(data, ["gmail", "slack", "summarize"]), []);
});

test("validateSkillReferences exempts the approval pseudo-skill", () => {
	const yaml = `name: with-approval
description: Approval step uses a pseudo-skill.
steps:
  - skill: approval
    prompt: Sign off before sending.
    requires_approval: true
`;
	const data = validateWorkflow(yaml).data!;
	assert.deepEqual(validateSkillReferences(data, ["gmail"]), []);
});

test("validateSkillReferences is a no-op when no skills are installed", () => {
	const data = validateWorkflow(UNKNOWN_SKILLS_YAML).data!;
	assert.deepEqual(validateSkillReferences(data, []), []);
});
