import type { Artifact, ArtifactKind } from "../../shared/types";

const KIND_ICON: Record<ArtifactKind, string> = {
	md: "≣",
	code: "‹›",
	image: "▦",
	pdf: "◫",
	html: "◐",
	other: "◇",
};

export function ArtifactsView(props: { artifacts: Artifact[]; onOpen: (path: string) => void }) {
	return (
		<div className="artifacts-view">
			<h1>Artifacts</h1>
			{props.artifacts.length === 0 && <div className="muted">No artifacts yet — the agent's files appear here.</div>}
			<div className="artifacts-grid">
				{props.artifacts.map((a) => (
					<button key={a.path} className="artifact-tile" onClick={() => props.onOpen(a.path)}>
						<span className="at-icon">{KIND_ICON[a.kind]}</span>
						<span className="at-name">{a.name}</span>
						<span className="at-path">{a.path}</span>
					</button>
				))}
			</div>
		</div>
	);
}
