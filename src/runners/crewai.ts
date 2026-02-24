import { writeFileSync, unlinkSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { exportToCrewAI } from '../adapters/crewai.js';
import { AgentManifest } from '../utils/loader.js';
import { error, info } from '../utils/format.js';

export function runWithCrewAI(agentDir: string, _manifest: AgentManifest): void {
  const config = exportToCrewAI(agentDir);
  const tmpFile = join(tmpdir(), `gitagent-${randomBytes(4).toString('hex')}.yaml`);

  writeFileSync(tmpFile, config, 'utf-8');

  info(`Running CrewAI agent from "${agentDir}"...`);

  try {
    const result = spawnSync('crewai', ['kickoff', '--config', tmpFile], {
      stdio: 'inherit',
      cwd: agentDir,
      env: { ...process.env },
    });

    if (result.error) {
      error(`Failed to run CrewAI: ${result.error.message}`);
      info('Make sure the crewai CLI is installed: pip install crewai');
      process.exit(1);
    }

    process.exit(result.status ?? 0);
  } finally {
    try { unlinkSync(tmpFile); } catch { /* ignore */ }
  }
}
