import { describe, it } from "node:test";
import assert from "node:assert";
import { createWebSearchTool } from "../dist/tools/web-search.js";

describe("Web Search Tool", () => {
	it("should create web search tool with correct configuration", () => {
		const tool = createWebSearchTool();
		
		assert.strictEqual(tool.name, "web_search");
		assert.strictEqual(tool.label, "web_search");
		assert.ok(tool.description.includes("You.com"));
		assert.ok(tool.parameters);
		assert.ok(tool.execute);
	});

	it("should have correct parameter schema", () => {
		const tool = createWebSearchTool();
		const schema = tool.parameters;
		
		assert.ok(schema.properties);
		assert.ok(schema.properties.query);
		assert.ok(schema.properties.count);
		assert.strictEqual(schema.properties.query.type, "string");
		assert.strictEqual(schema.properties.count.type, "number");
	});
});