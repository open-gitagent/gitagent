import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import type { ArtifactContent } from "../../shared/types";

export function ArtifactPreview(props: { content: ArtifactContent; onClose: () => void; onOpenSystem: () => void }) {
	const { content } = props;
	return (
		<section className="preview">
			<header className="preview-head">
				<span className="preview-name">{content.name}</span>
				<div className="spacer" />
				<button className="icon-btn" title="Open in system app" onClick={props.onOpenSystem}>
					⇱
				</button>
				<button className="icon-btn" title="Close" onClick={props.onClose}>
					×
				</button>
			</header>
			<div className="preview-body">{renderBody(content)}</div>
		</section>
	);
}

function renderBody(c: ArtifactContent) {
	if (c.kind === "image" && c.base64) {
		return <img className="preview-image" src={`data:${c.mime};base64,${c.base64}`} alt={c.name} />;
	}
	if (c.kind === "pdf" && c.base64) {
		return <embed className="preview-pdf" type="application/pdf" src={`data:application/pdf;base64,${c.base64}`} />;
	}
	if (c.kind === "html" && c.text !== undefined) {
		// Sandboxed: allow-same-origin only (no scripts, no top navigation, no network forms).
		return <iframe className="preview-html" sandbox="allow-same-origin" srcDoc={c.text} title={c.name} />;
	}
	if (c.kind === "md" && c.text !== undefined) {
		return (
			<div className="preview-md">
				<Markdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeHighlight]}>
					{c.text}
				</Markdown>
			</div>
		);
	}
	// code / other text
	return <pre className="preview-code">{c.text ?? "(no preview)"}</pre>;
}
