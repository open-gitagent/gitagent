import type { UIEvent, ArtifactKind } from "../../shared/types";

export type Item =
	| { kind: "user"; text: string }
	| { kind: "assistant"; text: string }
	| { kind: "tool"; toolName: string; args: Record<string, unknown>; result?: string; isError?: boolean }
	| { kind: "artifact"; name: string; path: string; akind: ArtifactKind }
	| { kind: "system"; subtype: string; text: string };

/** Pure reducer applied identically for live events and transcript replay. */
export function applyEvent(items: Item[], e: UIEvent): Item[] {
	switch (e.type) {
		case "user":
			return [...items, { kind: "user", text: e.text }];
		case "delta": {
			const last = items[items.length - 1];
			if (last && last.kind === "assistant") {
				return [...items.slice(0, -1), { ...last, text: last.text + e.text }];
			}
			return [...items, { kind: "assistant", text: e.text }];
		}
		case "tool_call":
			return [...items, { kind: "tool", toolName: e.toolName, args: e.args }];
		case "tool_result": {
			for (let i = items.length - 1; i >= 0; i--) {
				const it = items[i];
				if (it.kind === "tool" && it.toolName === e.toolName && it.result === undefined) {
					const copy = items.slice();
					copy[i] = { ...it, result: e.content, isError: e.isError };
					return copy;
				}
			}
			return items;
		}
		case "artifact":
			return [...items, { kind: "artifact", name: e.name, path: e.path, akind: e.kind }];
		case "system":
			return [...items, { kind: "system", subtype: e.subtype, text: e.content }];
		default:
			return items; // assistant_done / thinking / session_* are not transcript items
	}
}

export function reduceAll(events: UIEvent[]): Item[] {
	return events.reduce(applyEvent, [] as Item[]);
}
