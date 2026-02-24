import { writeFileSync, unlinkSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { exportToOpenAI } from '../adapters/openai.js';
import { AgentManifest } from '../utils/loader.js';
import { error, info } from '../utils/format.js';
import { resolveOpenAIKey } from '../utils/auth-provision.js';

export function runWithOpenAI(agentDir: string, _manifest: AgentManifest): void {
  if (!resolveOpenAIKey()) {
    error('OPENAI_API_KEY environment variable is not set');
    info('Set it with: export OPENAI_API_KEY="sk-..."');
    process.exit(1);
  }

  const script = exportToOpenAI(agentDir);
  const tmpFile = join(tmpdir(), `gitagent-${randomBytes(4).toString('hex')}.py`);

  writeFileSync(tmpFile, script, 'utf-8');

  info(`Running OpenAI agent from "${agentDir}"...`);

  try {
    const result = spawnSync('python3', [tmpFile], {
      stdio: 'inherit',
      cwd: agentDir,
      env: { ...process.env },
    });

    if (result.error) {
      error(`Failed to run Python: ${result.error.message}`);
      info('Make sure python3 is installed and the openai-agents package is available');
      process.exit(1);
    }

    process.exit(result.status ?? 0);
  } finally {
    try { unlinkSync(tmpFile); } catch { /* ignore */ }
  }
}
