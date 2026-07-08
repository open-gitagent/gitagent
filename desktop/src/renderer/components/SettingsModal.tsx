import { useState } from "react";
import type { AppSettings, PermissionMode } from "../../shared/types";

const KNOWN_MODELS = [
	"anthropic:claude-sonnet-4-5-20250929",
	"anthropic:claude-opus-4-6",
	"openai:gpt-4o",
	"openai:gpt-4o-mini",
	"google:gemini-2.0-flash-001",
];
const MODES: PermissionMode[] = ["plan", "default", "acceptEdits", "bypassPermissions"];
const KEY_VARS = ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY"];
const DEFAULT_GATEWAY = "http://localhost:8090/v1";
// A gateway virtual key is sent as the Bearer token for whichever provider the
// model prefix names, so it must be written to every provider's env var.
const GATEWAY_KEY_VARS = ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY", "LYZR_API_KEY"];

export function SettingsModal(props: {
	settings: AppSettings;
	onSave: (s: AppSettings) => void;
	onClose: () => void;
}) {
	const [local, setLocal] = useState<AppSettings>({ ...props.settings, keys: {} });
	const [virtualKey, setVirtualKey] = useState("");
	// Dummy on-device (Ollama) config — prototype, not applied yet.
	const [ollama, setOllama] = useState({
		enabled: false,
		host: "http://localhost:11434",
		model: "llama3.1:8b",
		apiKey: "ollama-local-xxxxxxxx",
	});
	const custom = !KNOWN_MODELS.includes(local.model);

	function handleSave() {
		// A virtual key authenticates against the OpenAI-compatible gateway. When
		// routing through the gateway, pi-ai sends the *selected provider's* env
		// key as the Bearer token (ANTHROPIC_API_KEY for anthropic:…, etc.), so
		// populate every provider var — whichever model is picked then works.
		const keys = { ...local.keys };
		if (virtualKey) {
			for (const v of GATEWAY_KEY_VARS) keys[v] = virtualKey;
		}
		props.onSave({ ...local, keys });
	}

	return (
		<div className="modal-overlay" onClick={props.onClose}>
			<div className="modal settings" onClick={(e) => e.stopPropagation()}>
				<div className="modal-title">Customize</div>

				<label>
					Model
					<select
						value={custom ? "custom" : local.model}
						onChange={(e) => setLocal({ ...local, model: e.target.value === "custom" ? "" : e.target.value })}
					>
						{KNOWN_MODELS.map((m) => (
							<option key={m} value={m}>{m}</option>
						))}
						<option value="custom">Custom (provider:model)</option>
					</select>
				</label>
				{custom && (
					<input
						placeholder="provider:model-id"
						value={local.model}
						onChange={(e) => setLocal({ ...local, model: e.target.value })}
					/>
				)}

				<label>
					Permission mode
					<select
						value={local.permissionMode}
						onChange={(e) => setLocal({ ...local, permissionMode: e.target.value as PermissionMode })}
					>
						{MODES.map((m) => (
							<option key={m} value={m}>{m}</option>
						))}
					</select>
				</label>

				<div className="keys-label">Virtual key (gateway)</div>
				<div className="keys-hint">
					Route every model through an OpenAI-compatible gateway with a single key.
				</div>
				<input
					placeholder={DEFAULT_GATEWAY}
					value={local.baseUrl ?? ""}
					onChange={(e) => setLocal({ ...local, baseUrl: e.target.value })}
				/>
				{!local.baseUrl && (
					<button className="link-btn" onClick={() => setLocal({ ...local, baseUrl: DEFAULT_GATEWAY })}>
						Use local gateway ({DEFAULT_GATEWAY})
					</button>
				)}
				<input
					type="password"
					placeholder="Virtual key"
					value={virtualKey}
					onChange={(e) => setVirtualKey(e.target.value)}
				/>

				<div className="keys-label">API keys (saved to ~/.gitagent/.env)</div>
				{KEY_VARS.map((k) => (
					<input
						key={k}
						type="password"
						placeholder={k}
						onChange={(e) => setLocal({ ...local, keys: { ...local.keys, [k]: e.target.value } })}
					/>
				))}

				<div className="ollama-head">
					<div>
						<div className="keys-label">On-device models · Ollama</div>
						<div className="keys-hint">
							Run open models fully on this device with Ollama — private and offline, no
							prompts or data leave your machine, and no cloud API bill.
						</div>
					</div>
					<button
						className={`sw-toggle${ollama.enabled ? " on" : ""}`}
						title={ollama.enabled ? "On-device enabled" : "On-device disabled"}
						onClick={() => setOllama({ ...ollama, enabled: !ollama.enabled })}
					>
						<span className="sw-knob" />
					</button>
				</div>
				<input
					placeholder="Ollama host"
					value={ollama.host}
					onChange={(e) => setOllama({ ...ollama, host: e.target.value })}
				/>
				<input
					placeholder="Local model (e.g. llama3.1:8b)"
					value={ollama.model}
					onChange={(e) => setOllama({ ...ollama, model: e.target.value })}
				/>
				<input
					type="password"
					placeholder="API key (optional for local)"
					value={ollama.apiKey}
					onChange={(e) => setOllama({ ...ollama, apiKey: e.target.value })}
				/>
				<div className="keys-hint ollama-note">Prototype — on-device settings aren't applied yet.</div>

				<div className="modal-actions">
					<button onClick={props.onClose}>Cancel</button>
					<button className="primary" onClick={handleSave}>Save</button>
				</div>
			</div>
		</div>
	);
}
