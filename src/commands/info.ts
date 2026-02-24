import { Command } from 'commander';
import { existsSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { loadAgentManifest, loadFileIfExists } from '../utils/loader.js';
import { success, error, info, heading, label, divider } from '../utils/format.js';

interface InfoOptions {
  dir: string;
}

export const infoCommand = new Command('info')
  .description('Display agent summary')
  .option('-d, --dir <dir>', 'Agent directory', '.')
  .action((options: InfoOptions) => {
    const dir = resolve(options.dir);

    let manifest;
    try {
      manifest = loadAgentManifest(dir);
    } catch (e) {
      error((e as Error).message);
      process.exit(1);
    }

    heading(`${manifest.name} v${manifest.version}`);
    console.log(`  ${manifest.description}`);
    divider();

    if (manifest.author) label('Author', manifest.author);
    if (manifest.license) label('License', manifest.license);

    if (manifest.model) {
      heading('Model');
      if (manifest.model.preferred) label('Preferred', manifest.model.preferred);
      if (manifest.model.fallback) label('Fallback', manifest.model.fallback.join(', '));
      if (manifest.model.constraints) {
        for (const [key, value] of Object.entries(manifest.model.constraints)) {
          label(`  ${key}`, String(value));
        }
      }
    }

    // Skills
    const skillsDir = join(dir, 'skills');
    if (existsSync(skillsDir)) {
      const skills = readdirSync(skillsDir, { withFileTypes: true })
        .filter(d => d.isDirectory())
        .map(d => d.name);
      if (skills.length > 0) {
        heading('Skills');
        skills.forEach(s => info(`  ${s}`));
      }
    }

    // Tools
    const toolsDir = join(dir, 'tools');
    if (existsSync(toolsDir)) {
      const tools = readdirSync(toolsDir)
        .filter(f => f.endsWith('.yaml'))
        .map(f => f.replace('.yaml', ''));
      if (tools.length > 0) {
        heading('Tools');
        tools.forEach(t => info(`  ${t}`));
      }
    }

    // Sub-agents
    const agentsDir = join(dir, 'agents');
    if (existsSync(agentsDir)) {
      const agents = readdirSync(agentsDir, { withFileTypes: true });
      const agentNames = [
        ...agents.filter(d => d.isDirectory()).map(d => d.name),
        ...agents.filter(f => f.isFile() && f.name.endsWith('.md')).map(f => f.name.replace('.md', '')),
      ];
      if (agentNames.length > 0) {
        heading('Sub-Agents');
        agentNames.forEach(a => info(`  ${a}`));
      }
    }

    // Runtime
    if (manifest.runtime) {
      heading('Runtime');
      if (manifest.runtime.max_turns) label('Max turns', String(manifest.runtime.max_turns));
      if (manifest.runtime.temperature !== undefined) label('Temperature', String(manifest.runtime.temperature));
      if (manifest.runtime.timeout) label('Timeout', `${manifest.runtime.timeout}s`);
    }

    // Compliance
    if (manifest.compliance) {
      const c = manifest.compliance;
      heading('Compliance');
      if (c.risk_tier) label('Risk Tier', c.risk_tier.toUpperCase());
      if (c.frameworks) label('Frameworks', c.frameworks.join(', '));
      if (c.supervision?.human_in_the_loop) label('Human-in-the-loop', c.supervision.human_in_the_loop);
      if (c.supervision?.designated_supervisor) label('Supervisor', c.supervision.designated_supervisor);
      if (c.recordkeeping?.audit_logging) label('Audit Logging', 'enabled');
      if (c.recordkeeping?.retention_period) label('Retention', c.recordkeeping.retention_period);
      if (c.model_risk?.inventory_id) label('Model Inventory ID', c.model_risk.inventory_id);
      if (c.model_risk?.validation_cadence) label('Validation Cadence', c.model_risk.validation_cadence);
      if (c.data_governance?.pii_handling) label('PII Handling', c.data_governance.pii_handling);
      if (c.data_governance?.data_classification) label('Data Classification', c.data_governance.data_classification);
    }

    // Tags
    if (manifest.tags && manifest.tags.length > 0) {
      heading('Tags');
      console.log(`  ${manifest.tags.join(', ')}`);
    }

    // SOUL.md preview
    const soul = loadFileIfExists(join(dir, 'SOUL.md'));
    if (soul) {
      heading('Soul (preview)');
      const lines = soul.split('\n').slice(0, 5);
      lines.forEach(l => console.log(`  ${l}`));
      if (soul.split('\n').length > 5) {
        console.log('  ...');
      }
    }

    console.log('');
  });
