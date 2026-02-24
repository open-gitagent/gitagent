import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import yaml from 'js-yaml';
import { loadAgentManifest, loadFileIfExists } from '../utils/loader.js';
import { loadAllSkills, getAllowedTools } from '../utils/skill-loader.js';

export function exportToClaudeCode(dir: string): string {
  const agentDir = resolve(dir);
  const manifest = loadAgentManifest(agentDir);

  // Build CLAUDE.md content
  const parts: string[] = [];

  parts.push(`# ${manifest.name}`);
  parts.push(`${manifest.description}\n`);

  // SOUL.md → identity section
  const soul = loadFileIfExists(join(agentDir, 'SOUL.md'));
  if (soul) {
    parts.push(soul);
  }

  // RULES.md → constraints section
  const rules = loadFileIfExists(join(agentDir, 'RULES.md'));
  if (rules) {
    parts.push(rules);
  }

  // Skills — loaded via skill-loader
  const skillsDir = join(agentDir, 'skills');
  const skills = loadAllSkills(skillsDir);
  if (skills.length > 0) {
    const skillParts: string[] = ['## Skills\n'];
    for (const skill of skills) {
      skillParts.push(`### ${skill.frontmatter.name}`);
      skillParts.push(skill.frontmatter.description);
      const tools = getAllowedTools(skill.frontmatter);
      if (tools.length > 0) {
        skillParts.push(`Allowed tools: ${tools.join(', ')}`);
      }
      skillParts.push('');
      skillParts.push(skill.instructions);
      skillParts.push('');
    }
    parts.push(skillParts.join('\n'));
  }

  // Model preferences as comments
  if (manifest.model?.preferred) {
    parts.push(`<!-- Model: ${manifest.model.preferred} -->`);
  }

  // Compliance constraints
  if (manifest.compliance) {
    const c = manifest.compliance;
    const complianceParts: string[] = ['## Compliance\n'];

    if (c.risk_tier) {
      complianceParts.push(`Risk Tier: ${c.risk_tier.toUpperCase()}`);
    }
    if (c.frameworks) {
      complianceParts.push(`Frameworks: ${c.frameworks.join(', ')}`);
    }
    if (c.supervision?.human_in_the_loop === 'always') {
      complianceParts.push('\n**All decisions require human approval.**');
    }
    if (c.communications?.fair_balanced) {
      complianceParts.push('- All outputs must be fair and balanced (FINRA 2210)');
    }
    if (c.communications?.no_misleading) {
      complianceParts.push('- Never make misleading or exaggerated statements');
    }
    if (c.data_governance?.pii_handling === 'redact') {
      complianceParts.push('- Redact all PII from outputs');
    }
    if (c.recordkeeping?.audit_logging) {
      complianceParts.push('- All actions are audit-logged');
    }

    parts.push(complianceParts.join('\n'));
  }

  // Knowledge (always_load)
  const knowledgeDir = join(agentDir, 'knowledge');
  const indexPath = join(knowledgeDir, 'index.yaml');
  if (existsSync(indexPath)) {
    const index = yaml.load(readFileSync(indexPath, 'utf-8')) as {
      documents?: Array<{ path: string; always_load?: boolean }>;
    };

    if (index.documents) {
      const alwaysLoad = index.documents.filter(d => d.always_load);
      for (const doc of alwaysLoad) {
        const content = loadFileIfExists(join(knowledgeDir, doc.path));
        if (content) {
          parts.push(`## Reference: ${doc.path}\n${content}`);
        }
      }
    }
  }

  return parts.join('\n\n');
}
