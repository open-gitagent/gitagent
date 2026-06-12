// Unit tests for src/utils/schemas.ts — the SkillFlow workflow validator.

import test from "node:test";
import assert from "node:assert/strict";

import { validateWorkflow, loadWorkflowSchema } from "../src/utils/schemas.ts";

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
