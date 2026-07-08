import { useRef, useState } from "react";

export function Composer(props: {
	model: string;
	running: boolean;
	onSend: (text: string) => void;
	onStop: () => void;
	onModelClick: () => void;
}) {
	const [text, setText] = useState("");
	// Attachments live in the UI only for now — not yet threaded to the agent.
	const [files, setFiles] = useState<File[]>([]);
	const [recording, setRecording] = useState(false);
	const fileInput = useRef<HTMLInputElement>(null);

	const send = () => {
		const t = text.trim();
		if (!t) return;
		props.onSend(t);
		setText("");
		setFiles([]);
	};

	const onPick = (e: React.ChangeEvent<HTMLInputElement>) => {
		const picked = Array.from(e.target.files ?? []);
		if (picked.length) setFiles((prev) => [...prev, ...picked]);
		e.target.value = ""; // let the same file be re-picked
	};

	return (
		<div className="composer">
			<div className="composer-card">
				{files.length > 0 && (
					<div className="composer-files">
						{files.map((f, i) => (
							<span className="file-chip" key={`${f.name}-${i}`} title={f.name}>
								<span className="fc-icon">📎</span>
								<span className="fc-name">{f.name}</span>
								<span className="fc-size">{formatSize(f.size)}</span>
								<button
									className="fc-x"
									title="Remove"
									onClick={() => setFiles((prev) => prev.filter((_, idx) => idx !== i))}
								>
									×
								</button>
							</span>
						))}
					</div>
				)}

				<textarea
					placeholder="Write a message…"
					value={text}
					rows={1}
					onChange={(e) => setText(e.target.value)}
					onKeyDown={(e) => {
						if (e.key === "Enter" && !e.shiftKey) {
							e.preventDefault();
							send();
						}
					}}
				/>
				<div className="composer-row">
					<input ref={fileInput} type="file" multiple hidden onChange={onPick} />
					<button className="cc-btn" title="Attach files" onClick={() => fileInput.current?.click()}>
						＋
					</button>
					<div className="spacer" />
					<button className="model-chip" onClick={props.onModelClick} title="Change model">
						{props.model} <span className="chev">▾</span>
					</button>
					<button
						className={`cc-btn${recording ? " active" : ""}`}
						title={recording ? "Stop recording" : "Voice (coming soon)"}
						onClick={() => setRecording((r) => !r)}
					>
						🎤
					</button>
					{props.running ? (
						<button className="send stop" onClick={props.onStop} title="Stop">
							■
						</button>
					) : (
						<button className="send" onClick={send} title="Send">
							↑
						</button>
					)}
				</div>
			</div>
			<div className="composer-hint">gitagent runs on your files — review actions in plan mode.</div>
		</div>
	);
}

function formatSize(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
