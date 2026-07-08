import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import type { Item } from "../lib/transcript";
import type { ArtifactKind } from "../../shared/types";

const KIND_ICON: Record<ArtifactKind, string> = {
	md: "≣",
	code: "‹›",
	image: "▦",
	pdf: "◫",
	html: "◐",
	other: "◇",
};

export function Message(props: { item: Item; streaming?: boolean; onOpenArtifact: (path: string) => void }) {
	const { item } = props;

	if (item.kind === "user") {
		return <div className="msg user">{item.text}</div>;
	}

	if (item.kind === "assistant") {
		// While streaming, render plain text — running react-markdown on every
		// token re-parses the whole growing message (O(n²)) and freezes the UI.
		// Markdown is applied once the message is complete.
		if (props.streaming) {
			return (
				<div className="msg assistant">
					<div className="stream-text">{item.text}</div>
				</div>
			);
		}
		return (
			<div className="msg assistant">
				<Markdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeHighlight]}>
					{item.text}
				</Markdown>
				<div className="msg-actions">
					<button title="Copy" onClick={() => navigator.clipboard.writeText(item.text)}>⧉</button>
					<button title="Good">👍</button>
					<button title="Bad">👎</button>
					<button title="Retry">↻</button>
				</div>
			</div>
		);
	}

	if (item.kind === "artifact") {
		return (
			<button className="artifact-card" onClick={() => props.onOpenArtifact(item.path)}>
				<span className="ac-icon">{KIND_ICON[item.akind]}</span>
				<span className="ac-body">
					<span className="ac-name">{item.name}</span>
					<span className="ac-sub">Created · click to preview</span>
				</span>
				<span className="ac-open">Open ▸</span>
			</button>
		);
	}

	if (item.kind === "system") {
		return <div className={`msg system ${item.subtype}`}>{item.text}</div>;
	}

	// tool
	return (
		<div className={`tool ${item.isError ? "error" : ""}`}>
			<div className="tool-head">
				<span className="tool-dot" /> {item.toolName}
				<span className="tool-args">{summarize(item.args)}</span>
			</div>
			{item.result !== undefined && item.result.trim() !== "" && (
				<pre className="tool-result">{item.result.slice(0, 4000)}</pre>
			)}
		</div>
	);
}

function summarize(args: Record<string, unknown>): string {
	const s = JSON.stringify(args);
	return s.length > 90 ? s.slice(0, 90) + "…" : s;
}
