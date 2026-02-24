import { Command } from 'commander';
import { existsSync, readFileSync, readdirSync, mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve, basename } from 'node:path';
import yaml from 'js-yaml';
import { error, heading, info, success, warn } from '../utils/format.js';

interface ImportOptions {
  from: string;
  dir: string;
}

function importFromClaude(sourcePath: string, targetDir: string): void {
  const sourceDir = resolve(sourcePath);

  // Look for CLAUDE.md
  const claudeMdPath = join(sourceDir, 'CLAUDE.md');
  if (!existsSync(claudeMdPath)) {
    throw new Error('CLAUDE.md not found in source directory');
  }

  const claudeMd = readFileSync(claudeMdPath, 'utf-8');

  // Create agent.yaml
  const dirName = basename(sourceDir);
  const agentYaml = {
    spec_version: '0.1.0',
    name: dirName.toLowerCase().replace(/[^a-z0-9-]/g, '-'),
    version: '0.1.0',
    description: `Imported from Claude Code project: ${dirName}`,
    model: { preferred: 'claude-sonnet-4-5-20250929' },
    skills: [] as string[],
    tools: [] as string[],
  };

  // Check for .claude directory with skills
  const claudeSkillsDir = join(sourceDir, '.claude', 'skills');
  if (existsSync(claudeSkillsDir)) {
    const skills = readdirSync(claudeSkillsDir, { withFileTypes: true });
    for (const entry of skills) {
      if (entry.isDirectory()) {
        agentYaml.skills.push(entry.name);
        const skillDir = join(targetDir, 'skills', entry.name);
        mkdirSync(skillDir, { recursive: true });

        // Copy skill files
        const skillFiles = readdirSync(join(claudeSkillsDir, entry.name));
        for (const file of skillFiles) {
          const content = readFileSync(join(claudeSkillsDir, entry.name, file), 'utf-8');
          writeFileSync(join(skillDir, file === `${entry.name}.md` ? 'SKILL.md' : file), content);
        }
        success(`Imported skill: ${entry.name}`);
      }
    }
  }

  // Write agent.yaml
  writeFileSync(join(targetDir, 'agent.yaml'), yaml.dump(agentYaml), 'utf-8');
  success('Created agent.yaml');

  // Convert CLAUDE.md to SOUL.md + RULES.md
  const sections = parseSections(claudeMd);
  let soulContent = '# Soul\n\n';
  let rulesContent = '# Rules\n\n';

  for (const [title, content] of sections) {
    const lower = title.toLowerCase();
    if (lower.includes('identity') || lower.includes('personality') || lower.includes('style') || lower.includes('about')) {
      soulContent += `## ${title}\n${content}\n\n`;
    } else if (lower.includes('rule') || lower.includes('constraint') || lower.includes('never') || lower.includes('always') || lower.includes('must')) {
      rulesContent += `## ${title}\n${content}\n\n`;
    } else {
      // Default to SOUL.md
      soulContent += `## ${title}\n${content}\n\n`;
    }
  }

  if (sections.length === 0) {
    soulContent += claudeMd;
  }

  writeFileSync(join(targetDir, 'SOUL.md'), soulContent, 'utf-8');
  success('Created SOUL.md');
  writeFileSync(join(targetDir, 'RULES.md'), rulesContent, 'utf-8');
  success('Created RULES.md');
}

function importFromCursor(sourcePath: string, targetDir: string): void {
  const sourceDir = resolve(sourcePath);

  // Look for .cursorrules or AGENTS.md
  let instructions = '';
  const cursorRulesPath = join(sourceDir, '.cursorrules');
  const agentsMdPath = join(sourceDir, 'AGENTS.md');

  if (existsSync(cursorRulesPath)) {
    instructions = readFileSync(cursorRulesPath, 'utf-8');
    info('Found .cursorrules');
  } else if (existsSync(agentsMdPath)) {
    instructions = readFileSync(agentsMdPath, 'utf-8');
    info('Found AGENTS.md');
  } else {
    throw new Error('No .cursorrules or AGENTS.md found in source directory');
  }

  const dirName = basename(sourceDir);
  const agentYaml = {
    spec_version: '0.1.0',
    name: dirName.toLowerCase().replace(/[^a-z0-9-]/g, '-'),
    version: '0.1.0',
    description: `Imported from Cursor project: ${dirName}`,
  };

  writeFileSync(join(targetDir, 'agent.yaml'), yaml.dump(agentYaml), 'utf-8');
  success('Created agent.yaml');

  writeFileSync(join(targetDir, 'SOUL.md'), `# Soul\n\n${instructions}`, 'utf-8');
  success('Created SOUL.md');

  writeFileSync(join(targetDir, 'AGENTS.md'), instructions, 'utf-8');
  success('Created AGENTS.md (preserved original)');
}

function importFromCrewAI(sourcePath: string, targetDir: string): void {
  // CrewAI uses YAML or Python for agent definitions
  const sourceFile = resolve(sourcePath);
  if (!existsSync(sourceFile)) {
    throw new Error(`Source file not found: ${sourceFile}`);
  }

  const content = readFileSync(sourceFile, 'utf-8');

  // Try to parse as YAML (CrewAI crew.yaml format)
  try {
    const crewConfig = yaml.load(content) as Record<string, unknown>;

    // Extract first agent definition
    const agents = crewConfig.agents as Record<string, { role?: string; goal?: string; backstory?: string }> | undefined;
    if (!agents) {
      throw new Error('No agents found in CrewAI config');
    }

    const [name, agentDef] = Object.entries(agents)[0];

    const agentYaml = {
      spec_version: '0.1.0',
      name: name.toLowerCase().replace(/[^a-z0-9-]/g, '-'),
      version: '0.1.0',
      description: agentDef.goal || `Imported from CrewAI: ${name}`,
    };

    writeFileSync(join(targetDir, 'agent.yaml'), yaml.dump(agentYaml), 'utf-8');
    success('Created agent.yaml');

    let soulContent = '# Soul\n\n';
    if (agentDef.role) soulContent += `## Core Identity\n${agentDef.role}\n\n`;
    if (agentDef.backstory) soulContent += `## Background\n${agentDef.backstory}\n\n`;
    if (agentDef.goal) soulContent += `## Purpose\n${agentDef.goal}\n\n`;

    writeFileSync(join(targetDir, 'SOUL.md'), soulContent, 'utf-8');
    success('Created SOUL.md');

    // Import additional agents as sub-agents
    const agentEntries = Object.entries(agents);
    if (agentEntries.length > 1) {
      mkdirSync(join(targetDir, 'agents'), { recursive: true });
      for (const [subName, subDef] of agentEntries.slice(1)) {
        const subDir = join(targetDir, 'agents', subName);
        mkdirSync(subDir, { recursive: true });
        const subAgentYaml = {
          spec_version: '0.1.0',
          name: subName.toLowerCase().replace(/[^a-z0-9-]/g, '-'),
          version: '0.1.0',
          description: subDef.goal || subName,
        };
        writeFileSync(join(subDir, 'agent.yaml'), yaml.dump(subAgentYaml), 'utf-8');

        let subSoul = '# Soul\n\n';
        if (subDef.role) subSoul += subDef.role + '\n';
        if (subDef.backstory) subSoul += '\n' + subDef.backstory + '\n';
        writeFileSync(join(subDir, 'SOUL.md'), subSoul, 'utf-8');

        success(`Imported sub-agent: ${subName}`);
      }
    }
  } catch (e) {
    throw new Error(`Failed to parse CrewAI config: ${(e as Error).message}`);
  }
}

function parseSections(markdown: string): [string, string][] {
  const sections: [string, string][] = [];
  const lines = markdown.split('\n');
  let currentTitle = '';
  let currentContent = '';

  for (const line of lines) {
    const headingMatch = line.match(/^#{1,3}\s+(.+)/);
    if (headingMatch) {
      if (currentTitle) {
        sections.push([currentTitle, currentContent.trim()]);
      }
      currentTitle = headingMatch[1];
      currentContent = '';
    } else {
      currentContent += line + '\n';
    }
  }

  if (currentTitle) {
    sections.push([currentTitle, currentContent.trim()]);
  }

  return sections;
}

export const importCommand = new Command('import')
  .description('Import from other agent formats')
  .requiredOption('--from <format>', 'Source format (claude, cursor, crewai)')
  .argument('<path>', 'Source file or directory path')
  .option('-d, --dir <dir>', 'Target directory', '.')
  .action((sourcePath: string, options: ImportOptions) => {
    const targetDir = resolve(options.dir);

    heading('Importing agent');
    info(`Format: ${options.from}`);
    info(`Source: ${sourcePath}`);

    try {
      switch (options.from) {
        case 'claude':
          importFromClaude(sourcePath, targetDir);
          break;
        case 'cursor':
          importFromCursor(sourcePath, targetDir);
          break;
        case 'crewai':
          importFromCrewAI(sourcePath, targetDir);
          break;
        default:
          error(`Unknown format: ${options.from}`);
          info('Supported formats: claude, cursor, crewai');
          process.exit(1);
      }

      success('\nImport complete');
      info('Run `gitagent validate` to check the imported agent');
    } catch (e) {
      error((e as Error).message);
      process.exit(1);
    }
  });
