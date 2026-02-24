import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import yaml from 'js-yaml';
import { loadAgentManifest, loadFileIfExists } from '../utils/loader.js';
import { loadAllSkills, getAllowedTools } from '../utils/skill-loader.js';

/**
 * Export a gitagent to OpenClaw workspace format.
 *
 * Returns a JSON object with all the files that should be written
 * to an OpenClaw workspace:
 *   - openclaw.json  (gateway/agent config)
 *   - AGENTS.md      (agent identity + rules + knowledge)
 *   - SOUL.md        (soul passthrough)
 *   - TOOLS.md       (tool definitions)
 *   - skills/<name>/SKILL.md (skill files, passed through)
 */
export interface OpenClawExport {
  config: object;
  agentsMd: string;
  soulMd: string;
  toolsMd: string;
  skills: Array<{ name: string; content: string }>;
}

export function exportToOpenClaw(dir: string): OpenClawExport {
  const agentDir = resolve(dir);
  const manifest = loadAgentManifest(agentDir);

  // --- openclaw.json config ---
  const config = buildOpenClawConfig(agentDir, manifest);

  // --- SOUL.md (passthrough) ---
  const soulMd = loadFileIfExists(join(agentDir, 'SOUL.md')) ?? `# ${manifest.name}\n${manifest.description}`;

  // --- AGENTS.md (identity + rules + knowledge + compliance) ---
  const agentsMd = buildAgentsMd(agentDir, manifest);

  // --- TOOLS.md (tool definitions) ---
  const toolsMd = buildToolsMd(agentDir);

  // --- Skills (passthrough SKILL.md files) ---
  const skills = collectSkills(agentDir);

  return { config, agentsMd, soulMd, toolsMd, skills };
}

/**
 * Export as a single string (for `gitagent export -f openclaw`).
 * Returns the openclaw.json + workspace files concatenated for display.
 */
export function exportToOpenClawString(dir: string): string {
  const exp = exportToOpenClaw(dir);
  const parts: string[] = [];

  parts.push('# === openclaw.json ===');
  parts.push(JSON.stringify(exp.config, null, 2));

  parts.push('\n# === workspace/AGENTS.md ===');
  parts.push(exp.agentsMd);

  parts.push('\n# === workspace/SOUL.md ===');
  parts.push(exp.soulMd);

  if (exp.toolsMd) {
    parts.push('\n# === workspace/TOOLS.md ===');
    parts.push(exp.toolsMd);
  }

  for (const skill of exp.skills) {
    parts.push(`\n# === workspace/skills/${skill.name}/SKILL.md ===`);
    parts.push(skill.content);
  }

  return parts.join('\n');
}

function buildOpenClawConfig(agentDir: string, manifest: ReturnType<typeof loadAgentManifest>): object {
  const config: Record<string, unknown> = {
    agent: {
      model: mapModelName(manifest.model?.preferred ?? 'anthropic/claude-sonnet-4-5-20250929'),
      workspace: '~/.openclaw/workspace',
    },
  };

  // Map runtime settings
  if (manifest.runtime) {
    const agentConfig = config.agent as Record<string, unknown>;
    if (manifest.runtime.temperature !== undefined) {
      agentConfig.temperature = manifest.runtime.temperature;
    }
    if (manifest.runtime.max_turns !== undefined) {
      agentConfig.maxTurns = manifest.runtime.max_turns;
    }
  }

  // Map model constraints
  if (manifest.model?.constraints?.max_tokens) {
    (config.agent as Record<string, unknown>).maxTokens = manifest.model.constraints.max_tokens;
  }

  return config;
}

/**
 * Map gitagent model names to OpenClaw provider/model format.
 * OpenClaw uses "anthropic/claude-opus-4-6" style names.
 */
function mapModelName(model: string): string {
  if (model.startsWith('anthropic/') || model.startsWith('openai/')) {
    return model;
  }
  if (model.startsWith('claude-')) {
    return `anthropic/${model}`;
  }
  if (model.startsWith('gpt-') || model.startsWith('o1') || model.startsWith('o3')) {
    return `openai/${model}`;
  }
  return model;
}

function buildAgentsMd(agentDir: string, manifest: ReturnType<typeof loadAgentManifest>): string {
  const parts: string[] = [];

  // Agent identity header
  parts.push(`# ${manifest.name} v${manifest.version}`);
  parts.push(`${manifest.description}\n`);

  if (manifest.author) {
    parts.push(`Author: ${manifest.author}`);
  }

  // RULES.md
  const rules = loadFileIfExists(join(agentDir, 'RULES.md'));
  if (rules) {
    parts.push('');
    parts.push(rules);
  }

  // Knowledge documents (always_load)
  const knowledgeDir = join(agentDir, 'knowledge');
  const indexPath = join(knowledgeDir, 'index.yaml');
  if (existsSync(indexPath)) {
    const index = yaml.load(readFileSync(indexPath, 'utf-8')) as {
      documents?: Array<{ path: string; always_load?: boolean }>;
    };

    if (index.documents) {
      const alwaysLoad = index.documents.filter(d => d.always_load);
      if (alwaysLoad.length > 0) {
        parts.push('\n## Knowledge Base\n');
        for (const doc of alwaysLoad) {
          const content = loadFileIfExists(join(knowledgeDir, doc.path));
          if (content) {
            parts.push(`### ${doc.path}\n${content}\n`);
          }
        }
      }
    }
  }

  // Compliance constraints
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
      constraints.push('- All communications must be fair and balanced');
    }
    if (c.communications?.no_misleading) {
      constraints.push('- Never make misleading, exaggerated, or promissory statements');
    }
    if (c.data_governance?.pii_handling === 'redact') {
      constraints.push('- Redact all PII from outputs');
    }
    if (c.data_governance?.pii_handling === 'prohibit') {
      constraints.push('- Do not process any personally identifiable information');
    }

    if (constraints.length > 0) {
      parts.push(`\n## Compliance Constraints\n${constraints.join('\n')}`);
    }
  }

  // Sub-agents
  if (manifest.agents) {
    parts.push('\n## Sub-Agents\n');
    for (const [name, config] of Object.entries(manifest.agents)) {
      parts.push(`### ${name}`);
      if (config.description) parts.push(config.description);
      if (config.delegation?.mode) parts.push(`Delegation: ${config.delegation.mode}`);
      if (config.delegation?.triggers) parts.push(`Triggers: ${config.delegation.triggers.join(', ')}`);
      parts.push('');
    }
  }

  return parts.join('\n');
}

function buildToolsMd(agentDir: string): string {
  const toolsDir = join(agentDir, 'tools');
  if (!existsSync(toolsDir)) return '';

  const files = readdirSync(toolsDir).filter(f => f.endsWith('.yaml'));
  if (files.length === 0) return '';

  const parts: string[] = ['# Tools\n'];

  for (const file of files) {
    try {
      const content = readFileSync(join(toolsDir, file), 'utf-8');
      const toolConfig = yaml.load(content) as {
        name: string;
        description: string;
        input_schema?: {
          properties?: Record<string, { type: string; description?: string }>;
          required?: string[];
        };
      };

      parts.push(`## ${toolConfig.name}`);
      parts.push(`${toolConfig.description}\n`);

      if (toolConfig.input_schema?.properties) {
        parts.push('**Parameters:**');
        for (const [name, schema] of Object.entries(toolConfig.input_schema.properties)) {
          const required = toolConfig.input_schema.required?.includes(name) ? ' (required)' : '';
          parts.push(`- \`${name}\` (${schema.type})${required}${schema.description ? ` — ${schema.description}` : ''}`);
        }
        parts.push('');
      }
    } catch { /* skip malformed tools */ }
  }

  return parts.join('\n');
}

function collectSkills(agentDir: string): Array<{ name: string; content: string }> {
  const skills: Array<{ name: string; content: string }> = [];
  const skillsDir = join(agentDir, 'skills');
  if (!existsSync(skillsDir)) return skills;

  const entries = readdirSync(skillsDir, { withFileTypes: true });
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const skillMdPath = join(skillsDir, entry.name, 'SKILL.md');
    if (!existsSync(skillMdPath)) continue;

    skills.push({
      name: entry.name,
      content: readFileSync(skillMdPath, 'utf-8'),
    });
  }

  return skills;
}
