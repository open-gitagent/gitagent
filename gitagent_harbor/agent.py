"""Harbor installed agent that runs the static gitagent binary in the task container.

Usage (from the repository root, after building the musl binaries):

    PYTHONPATH=$PWD harbor run -d terminal-bench/terminal-bench-2-1 \
        -a gitagent_harbor.agent:GitAgent -m anthropic/<model> ...

Agent kwargs (``--ak key=value``):
    timeout_s   wall-clock budget passed to ``--timeout-s``; set it a little
                below Harbor's agent timeout so the run ends cleanly
    profile     built-in profile name, or a path to a profile YAML file on the
                host (uploaded into the container), passed to ``--profile``
    binary_dir  directory holding ``<target>/release/gitagent`` (default: ./target)
    script      host path of a scripted-provider fixture; used with
                ``-m scripted/<anything>`` for dry runs without a model API

The agent never touches ``/tests``.
"""

from __future__ import annotations

import json
import shlex
from pathlib import Path

from harbor.agents.capabilities import AgentCapabilities
from harbor.agents.installed.base import BaseInstalledAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

REPO_ROOT = Path(__file__).resolve().parent.parent
REMOTE_DIR = "/opt/gitagent"
REMOTE_BIN = f"{REMOTE_DIR}/gitagent"
REMOTE_INSTRUCTION = f"{REMOTE_DIR}/instruction.md"
REMOTE_SCRIPT = f"{REMOTE_DIR}/script.yaml"
REMOTE_PROFILE = f"{REMOTE_DIR}/profile.yaml"

TARGETS = {
    "x86_64": "x86_64-unknown-linux-musl",
    "amd64": "x86_64-unknown-linux-musl",
    "aarch64": "aarch64-unknown-linux-musl",
    "arm64": "aarch64-unknown-linux-musl",
}

# Only the key for the model's provider is passed into the container.
PROVIDER_KEYS = {
    "anthropic": ("ANTHROPIC_API_KEY",),
    "openai": ("OPENAI_API_KEY",),
    "google": ("GEMINI_API_KEY", "GOOGLE_API_KEY"),
    "gemini": ("GEMINI_API_KEY", "GOOGLE_API_KEY"),
    "openrouter": ("OPENROUTER_API_KEY",),
}


class GitAgent(BaseInstalledAgent):
    capabilities = AgentCapabilities(atif=True)

    def __init__(
        self,
        *args,
        timeout_s: int | float | str | None = None,
        profile: str | None = None,
        binary_dir: str | None = None,
        script: str | None = None,
        **kwargs,
    ):
        super().__init__(*args, **kwargs)
        self._timeout_s = int(float(timeout_s)) if timeout_s else None
        self._profile = profile
        self._binary_dir = Path(binary_dir) if binary_dir else REPO_ROOT / "target"
        self._script = Path(script) if script else None

    @staticmethod
    def name() -> str:
        return "gitagent"

    async def install(self, environment: BaseEnvironment) -> None:
        arch = (await self.exec_as_root(environment, "uname -m")).stdout.strip()
        target = TARGETS.get(arch)
        if target is None:
            raise RuntimeError(f"unsupported container architecture: {arch!r}")
        local = self._binary_dir / target / "release" / "gitagent"
        if not local.is_file():
            raise FileNotFoundError(
                f"{local} not found; run scripts/build-release.sh first"
            )
        # Images come in both architectures, so every trial must get the same
        # build: refuse a binary that is older than its sibling for the other
        # architecture (someone rebuilt only one target).
        for other in set(TARGETS.values()) - {target}:
            sibling = self._binary_dir / other / "release" / "gitagent"
            if sibling.is_file() and sibling.stat().st_mtime - local.stat().st_mtime > 600:
                raise RuntimeError(
                    f"{local} is older than {sibling}; run scripts/build-release.sh "
                    "with no arguments so both architectures carry the same code"
                )
        await self.exec_as_root(environment, f"mkdir -p {REMOTE_DIR}")
        await environment.upload_file(local, REMOTE_BIN)
        await self.exec_as_root(environment, f"chmod 755 {REMOTE_DIR} {REMOTE_BIN}")
        version = await self.exec_as_agent(environment, f"{REMOTE_BIN} --version")
        self._version = version.stdout.strip().split()[-1]

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        await self._upload_config_text(
            environment,
            content=self.render_instruction(instruction),
            remote_path=REMOTE_INSTRUCTION,
            filename="instruction.md",
        )

        provider, model = self._split_model()
        env: dict[str, str] = {}
        if provider == "scripted":
            if self._script is None:
                raise ValueError("model 'scripted/...' needs --ak script=<fixture path>")
            await self._upload_config_text(
                environment,
                content=self._script.read_text(),
                remote_path=REMOTE_SCRIPT,
                filename="script.yaml",
            )
            model_arg = f"scripted:{REMOTE_SCRIPT}"
        else:
            model_arg = f"{provider}:{model}"
            upper = provider.upper().replace("-", "_")
            # Known providers have fixed key names; other (OpenAI-compatible)
            # endpoints use {NAME}_API_KEY and {NAME}_BASE_URL.
            keys = PROVIDER_KEYS.get(
                provider, (f"{upper}_API_KEY", f"{upper}_BASE_URL", "GITAGENT_API_KEY")
            )
            for key in keys:
                value = self._get_env(key)
                if value:
                    env[key] = value

        logs = str(self.environment_logs_dir)
        argv = [
            REMOTE_BIN,
            "--bench",
            "--instruction-file",
            REMOTE_INSTRUCTION,
            "--model",
            model_arg,
            "--out-dir",
            logs,
        ]
        if self._profile:
            profile = self._profile
            local = Path(profile)
            if local.suffix in (".yaml", ".yml") and local.is_file():
                await self._upload_config_text(
                    environment,
                    content=local.read_text(),
                    remote_path=REMOTE_PROFILE,
                    filename="profile.yaml",
                )
                profile = REMOTE_PROFILE
            argv += ["--profile", profile]
        if self._timeout_s:
            argv += ["--timeout-s", str(self._timeout_s)]
        if getattr(self, "session_id", None):
            argv += ["--session-id", str(self.session_id)]
        argv.append("--linger")

        await self.exec_as_agent(environment, self._launch_command(argv, logs), env=env)

    @staticmethod
    def _launch_command(argv: list[str], logs: str) -> str:
        """Starts gitagent detached and returns once it writes its `done` file.

        gitagent keeps its shells open afterwards (--linger), so servers and
        jobs the agent left running are still up when the tests run.
        """
        q_logs = shlex.quote(logs)
        done = shlex.quote(f"{logs}/done")
        log_file = shlex.quote(f"{logs}/gitagent.log")
        trajectory = shlex.quote(f"{logs}/trajectory.json")
        # A launch is retried once when the process dies before it has
        # written anything (no log, no trajectory): that is the emulator or
        # the container failing at startup, not the agent's run. A crash
        # after the run has started is reported as it is.
        return (
            f"mkdir -p {q_logs} && rm -f {done}; "
            f"if command -v nohup >/dev/null 2>&1; then n=nohup; else n=; fi; "
            f"attempt=0; while :; do attempt=$((attempt+1)); "
            f"$n {shlex.join(argv)} >{log_file} 2>&1 </dev/null & pid=$!; "
            f"while [ ! -f {done} ] && kill -0 $pid 2>/dev/null; do sleep 1; done; "
            f"if [ -f {done} ]; then break; fi; "
            f"wait $pid; code=$?; "
            f"if [ $attempt -lt 2 ] && [ $code -ge 128 ] && [ ! -s {log_file} ] && [ ! -f {trajectory} ]; then "
            f'echo "gitagent died with status $code before starting; launching again" >&2; sleep 2; continue; fi; '
            f"cat {log_file}; exit $code; done; "
            f"cat {log_file}; "
            f'exit "$(head -n 1 {done})"'
        )

    def populate_context_post_run(self, context: AgentContext) -> None:
        path = self.logs_dir / "trajectory.json"
        if not path.is_file():
            return
        trajectory = json.loads(path.read_text())
        # Summed per step so a run cut off by a timeout still reports usage.
        prompt = completion = cached = 0
        for step in trajectory.get("steps", []):
            metrics = step.get("metrics") or {}
            prompt += metrics.get("prompt_tokens") or 0
            completion += metrics.get("completion_tokens") or 0
            cached += metrics.get("cached_tokens") or 0
        context.n_input_tokens = prompt
        context.n_output_tokens = completion
        context.n_cache_tokens = cached

        metadata = {"outcome": (trajectory.get("extra") or {}).get("outcome")}
        counters = self.logs_dir / "counters.json"
        if counters.is_file():
            metadata["counters"] = json.loads(counters.read_text())
        context.metadata = metadata

    def _split_model(self) -> tuple[str, str]:
        if not self.model_name:
            raise ValueError("gitagent needs a model: -m provider/model")
        provider, _, model = self.model_name.partition("/")
        if not model:
            raise ValueError(f"model must look like provider/model, got {self.model_name!r}")
        return provider, model
