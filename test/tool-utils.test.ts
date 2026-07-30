import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

let toAgentTool: typeof import("../dist/tool-utils.js").toAgentTool;

before(async () => {
	({ toAgentTool } = await import("../dist/tool-utils.js"));
});

const def = (handler: any) => ({
	name: "echo",
	description: "Echoes input",
	inputSchema: { properties: { text: { type: "string" } }, required: ["text"] },
	handler,
});

describe("toAgentTool", () => {
	it("maps GCToolDefinition fields onto an AgentTool with a built schema", () => {
		const t = toAgentTool(def(async (a: any) => a.text));
		assert.equal(t.name, "echo");
		assert.equal(t.label, "echo");
		assert.equal(t.description, "Echoes input");
		assert.equal(t.parameters.type, "object");
		assert.ok(t.parameters.properties.text);
		assert.equal(typeof t.execute, "function");
	});

	it("wraps a string handler result as text content", async () => {
		const t = toAgentTool(def(async (a: any) => `got: ${a.text}`));
		const res = await t.execute("call-1", { text: "hi" });
		assert.deepEqual(res.content, [{ type: "text", text: "got: hi" }]);
		assert.equal(res.details, undefined);
	});

	it("passes through text and details from an object handler result", async () => {
		const t = toAgentTool(def(async () => ({ text: "done", details: { rows: 3 } })));
		const res = await t.execute("call-1", { text: "x" });
		assert.equal(res.content[0].text, "done");
		assert.deepEqual(res.details, { rows: 3 });
	});

	it("forwards the abort signal to the handler", async () => {
		let received: AbortSignal | undefined;
		const t = toAgentTool(def(async (_a: any, signal?: AbortSignal) => {
			received = signal;
			return "ok";
		}));
		const ac = new AbortController();
		await t.execute("call-1", { text: "x" }, ac.signal);
		assert.equal(received, ac.signal);
	});
});
