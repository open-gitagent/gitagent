import { writeFileSync, unlinkSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { exportToLangChain, detectProvider } from '../adapters/langchain.js';
import { AgentManifest } from '../utils/loader.js';
import { error, info } from '../utils/format.js';

export function runWithLangChain(agentDir: string, _manifest: AgentManifest): void {
  const model = _manifest.model?.preferred ?? 'gpt-4o';
  const providerInfo = detectProvider(model);

  // Check the appropriate API key env var
  const apiKey = process.env[providerInfo.envVar];
  if (!apiKey) {
    error(`${providerInfo.envVar} environment variable is not set`);
    info(`Set it with: export ${providerInfo.envVar}="your-key-here"`);
    process.exit(1);
  }

  const script = exportToLangChain(agentDir);
  const tmpFile = join(tmpdir(), `gitagent-langchain-${randomBytes(4).toString('hex')}.py`);

  writeFileSync(tmpFile, script, 'utf-8');

  const pipHint = `langchain ${providerInfo.pipPackage}`;
  info(`Running LangChain agent from "${agentDir}"...`);
  info(`Requires: pip install ${pipHint}`);

  try {
    const result = spawnSync('python3', [tmpFile], {
      stdio: 'inherit',
      cwd: agentDir,
      env: { ...process.env },
    });

    if (result.error) {
      error(`Failed to run Python: ${result.error.message}`);
      info('Make sure python3 is installed and langchain packages are available:');
      info(`  pip install ${pipHint}`);
      process.exit(1);
    }

    process.exit(result.status ?? 0);
  } finally {
    try { unlinkSync(tmpFile); } catch { /* ignore */ }
  }
}
