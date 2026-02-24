import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import yaml from 'js-yaml';
import { loadAgentManifest, loadFileIfExists } from '../utils/loader.js';
import { loadAllSkills, getAllowedTools } from '../utils/skill-loader.js';

export function exportToSystemPrompt(dir: string): string {
  const agentDir = resolve(dir);
  const manifest = loadAgentManifest(agentDir);
  const parts: string[] = [];

  // Agent identity header
  parts.push(`# ${manifest.name} v${manifest.version}`);
  parts.push(`${manifest.description}\n`);

  // SOUL.md
  const soul = loadFileIfExists(join(agentDir, 'SOUL.md'));
  if (soul) {
    parts.push(soul);
  }

  // RULES.md
  const rules = loadFileIfExists(join(agentDir, 'RULES.md'));
  if (rules) {
    parts.push(rules);
  }

  // Skills — loaded via skill-loader
  const skillsDir = join(agentDir, 'skills');
  const skills = loadAllSkills(skillsDir);
  for (const skill of skills) {
    const toolsList = getAllowedTools(skill.frontmatter);
    const toolsNote = toolsList.length > 0 ? `\nAllowed tools: ${toolsList.join(', ')}` : '';
    parts.push(`## Skill: ${skill.frontmatter.name}\n${skill.frontmatter.description}${toolsNote}\n\n${skill.instructions}`);
  }

  // Knowledge (always_load documents)
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
          parts.push(`## Knowledge: ${doc.path}\n${content}`);
        }
      }
    }
  }

  // Compliance constraints as system instructions
  if (manifest.compliance) {
    const c = manifest.compliance;
    const constraints: string[] = [];

    if (c.supervision?.human_in_the_loop === 'always') {
      constraints.push('- All decisions require human approval before execution');
    }
    if (c.supervision?.escalation_triggers) {
      constraints.push('- Escalate to human supervisor when:');
      for (const trigger of c.supervision.escalation_triggers) {
        for (const [key, value] of Object.entries(trigger)) {
          constraints.push(`  - ${key}: ${value}`);
        }
      }
    }
    if (c.communications?.fair_balanced) {
      constraints.push('- All communications must be fair and balanced (FINRA 2210)');
    }
    if (c.communications?.no_misleading) {
      constraints.push('- Never make misleading, exaggerated, or promissory statements');
    }
    if (c.data_governance?.pii_handling === 'redact') {
      constraints.push('- Redact all PII from outputs and intermediate reasoning');
    }
    if (c.data_governance?.pii_handling === 'prohibit') {
      constraints.push('- Do not process any personally identifiable information');
    }

    if (constraints.length > 0) {
      parts.push(`## Compliance Constraints\n${constraints.join('\n')}`);
    }
  }

  // Memory
  const memory = loadFileIfExists(join(agentDir, 'memory', 'MEMORY.md'));
  if (memory && memory.trim().split('\n').length > 2) {
    parts.push(`## Memory\n${memory}`);
  }

  return parts.join('\n\n');
}
