import { writeFileSync, mkdirSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { exportToOpenClaw } from '../adapters/openclaw.js';
import { AgentManifest } from '../utils/loader.js';
import { error, info, success, warn } from '../utils/format.js';
import { ensureOpenClawAuth } from '../utils/auth-provision.js';

export interface OpenClawRunOptions {
  prompt?: string;
}

export function runWithOpenClaw(agentDir: string, manifest: AgentManifest, options: OpenClawRunOptions = {}): void {
  ensureOpenClawAuth();
  const exp = exportToOpenClaw(agentDir);

  // Create a temporary workspace
  const workspaceDir = join(tmpdir(), `gitagent-openclaw-${randomBytes(4).toString('hex')}`);
  mkdirSync(workspaceDir, { recursive: true });

  // Write workspace files
  writeFileSync(join(workspaceDir, 'AGENTS.md'), exp.agentsMd, 'utf-8');
  writeFileSync(join(workspaceDir, 'SOUL.md'), exp.soulMd, 'utf-8');

  if (exp.toolsMd) {
    writeFileSync(join(workspaceDir, 'TOOLS.md'), exp.toolsMd, 'utf-8');
  }

  // Write skills
  for (const skill of exp.skills) {
    const skillDir = join(workspaceDir, 'skills', skill.name);
    mkdirSync(skillDir, { recursive: true });
    writeFileSync(join(skillDir, 'SKILL.md'), skill.content, 'utf-8');
  }

  // Write openclaw.json config, pointing workspace to our temp dir
  const config = exp.config as Record<string, Record<string, unknown>>;
  config.agent = config.agent ?? {};
  config.agent.workspace = workspaceDir;

  const configFile = join(workspaceDir, 'openclaw.json');
  writeFileSync(configFile, JSON.stringify(config, null, 2), 'utf-8');

  info(`Workspace prepared at ${workspaceDir}`);
  info(`  AGENTS.md, SOUL.md${exp.toolsMd ? ', TOOLS.md' : ''}`);
  if (exp.skills.length > 0) {
    info(`  Skills: ${exp.skills.map(s => s.name).join(', ')}`);
  }

  // OpenClaw agent requires --message
  if (!options.prompt) {
    error('OpenClaw requires a prompt. Use -p "your message" to provide one.');
    info('Example: gitagent run -r <url> -a openclaw -p "Review my auth module"');
    try { rmSync(workspaceDir, { recursive: true, force: true }); } catch { /* ignore */ }
    process.exit(1);
  }

  // Build openclaw CLI args
  // --local runs embedded agent, --session-id provides an ad-hoc session
  const sessionId = `gitagent-${manifest.name}-${randomBytes(4).toString('hex')}`;
  const args: string[] = ['agent', '--local', '--session-id', sessionId, '--message', options.prompt];

  // Map thinking level from model constraints
  if (manifest.compliance?.supervision?.human_in_the_loop === 'always') {
    args.push('--thinking', 'high');
    info('Compliance: human_in_the_loop=always → thinking=high');
  }

  info(`Launching OpenClaw agent "${manifest.name}"...`);

  try {
    const result = spawnSync('openclaw', args, {
      stdio: 'inherit',
      cwd: workspaceDir,
      env: {
        ...process.env,
        OPENCLAW_CONFIG: configFile,
      },
    });

    if (result.error) {
      error(`Failed to launch OpenClaw: ${result.error.message}`);
      info('Make sure OpenClaw is installed: npm install -g openclaw@latest');
      process.exit(1);
    }

    process.exit(result.status ?? 0);
  } finally {
    // Cleanup temp workspace
    try { rmSync(workspaceDir, { recursive: true, force: true }); } catch { /* ignore */ }
  }
}
