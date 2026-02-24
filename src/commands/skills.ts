import { Command } from 'commander';
import { resolve, join } from 'node:path';
import { existsSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { success, error, info, heading, divider, label } from '../utils/format.js';
import { discoverSkills } from '../utils/skill-discovery.js';
import { loadSkillFull, getAllowedTools } from '../utils/skill-loader.js';
import { createProvider, getDefaultProvider, type SkillRegistryProvider } from '../utils/registry-provider.js';
import { loadAgentManifest } from '../utils/loader.js';

function getProvider(providerName?: string, dir?: string): SkillRegistryProvider {
  if (providerName) {
    return createProvider({ name: providerName });
  }

  // Check agent.yaml for registry config
  if (dir) {
    try {
      const manifest = loadAgentManifest(dir);
      const registries = (manifest as unknown as Record<string, unknown>).registries as Array<{ name: string; url?: string }> | undefined;
      if (registries && registries.length > 0) {
        return createProvider(registries[0]);
      }
    } catch {
      // No agent.yaml or no registries — use default
    }
  }

  return getDefaultProvider();
}

const searchCommand = new Command('search')
  .description('Search for skills in a registry')
  .argument('<query>', 'Search query')
  .option('-p, --provider <provider>', 'Registry provider (skillsmp, github)')
  .option('-d, --dir <dir>', 'Agent directory', '.')
  .option('-l, --limit <limit>', 'Max results', '20')
  .action(async (query: string, options: { provider?: string; dir: string; limit: string }) => {
    const provider = getProvider(options.provider, resolve(options.dir));
    heading(`Searching ${provider.name} for "${query}"`);

    try {
      const results = await provider.search(query, { limit: parseInt(options.limit) });

      if (results.items.length === 0) {
        info('No skills found');
        return;
      }

      info(`Found ${results.total} result${results.total !== 1 ? 's' : ''}`);
      divider();

      for (const item of results.items) {
        console.log(`  ${item.name} (v${item.version})`);
        console.log(`    ${item.description}`);
        if (item.author) console.log(`    by ${item.author}`);
        if (item.downloads !== undefined) console.log(`    ${item.downloads} downloads`);
        console.log();
      }
    } catch (e) {
      error((e as Error).message);
      process.exit(1);
    }
  });

const installCommand = new Command('install')
  .description('Install a skill from a registry')
  .argument('<name>', 'Skill name or reference (e.g., "pdf-reader", "owner/repo#skills/pdf")')
  .option('-p, --provider <provider>', 'Registry provider (skillsmp, github, local)')
  .option('-g, --global', 'Install to personal skills (~/.agents/skills/)', false)
  .option('-d, --dir <dir>', 'Agent directory', '.')
  .action(async (name: string, options: { provider?: string; global: boolean; dir: string }) => {
    const agentDir = resolve(options.dir);
    const provider = getProvider(options.provider, agentDir);

    let targetDir: string;
    if (options.global) {
      targetDir = join(homedir(), '.agents', 'skills');
    } else {
      targetDir = join(agentDir, 'skills');
    }

    mkdirSync(targetDir, { recursive: true });

    heading(`Installing skill "${name}" from ${provider.name}`);

    try {
      await provider.install(name, targetDir);
      success(`Installed to ${options.global ? '~/.agents/skills/' : 'skills/'}`);
    } catch (e) {
      error((e as Error).message);
      process.exit(1);
    }
  });

const listCommand = new Command('list')
  .description('List discovered skills')
  .option('-d, --dir <dir>', 'Agent directory', '.')
  .option('-l, --local', 'Only show agent-local skills', false)
  .action((options: { dir: string; local: boolean }) => {
    const agentDir = resolve(options.dir);
    const skills = discoverSkills({ agentDir, localOnly: options.local });

    heading('Discovered Skills');

    if (skills.length === 0) {
      info('No skills found');
      return;
    }

    divider();
    for (const skill of skills) {
      console.log(`  ${skill.name}`);
      console.log(`    ${skill.description}`);
      console.log(`    source: ${skill.source} (${skill.directory})`);
      if (skill.license) console.log(`    license: ${skill.license}`);
      console.log();
    }
    info(`${skills.length} skill${skills.length !== 1 ? 's' : ''} found`);
  });

const infoCommandDef = new Command('info')
  .description('Show detailed information about a skill')
  .argument('<name>', 'Skill name')
  .option('-d, --dir <dir>', 'Agent directory', '.')
  .action((name: string, options: { dir: string }) => {
    const agentDir = resolve(options.dir);
    const skills = discoverSkills({ agentDir });
    const skill = skills.find(s => s.name === name);

    if (!skill) {
      error(`Skill "${name}" not found`);
      process.exit(1);
    }

    const skillMdPath = join(skill.directory, 'SKILL.md');
    let parsed;
    try {
      parsed = loadSkillFull(skillMdPath);
    } catch (e) {
      error(`Failed to load skill: ${(e as Error).message}`);
      process.exit(1);
    }

    heading(`Skill: ${parsed.frontmatter.name}`);
    divider();
    label('Name', parsed.frontmatter.name);
    label('Description', parsed.frontmatter.description);
    label('Source', skill.source);
    label('Directory', skill.directory);

    if (parsed.frontmatter.license) {
      label('License', parsed.frontmatter.license);
    }
    if (parsed.frontmatter.compatibility) {
      label('Compatibility', parsed.frontmatter.compatibility);
    }

    const tools = getAllowedTools(parsed.frontmatter);
    if (tools.length > 0) {
      label('Allowed Tools', tools.join(', '));
    }

    if (parsed.frontmatter.metadata && Object.keys(parsed.frontmatter.metadata).length > 0) {
      console.log();
      info('Metadata:');
      for (const [key, value] of Object.entries(parsed.frontmatter.metadata)) {
        label(`  ${key}`, value);
      }
    }

    // Show optional directories
    const dirs: string[] = [];
    if (parsed.hasScripts) dirs.push('scripts/');
    if (parsed.hasReferences) dirs.push('references/');
    if (parsed.hasAssets) dirs.push('assets/');
    if (parsed.hasAgents) dirs.push('agents/');
    if (dirs.length > 0) {
      console.log();
      info(`Optional directories: ${dirs.join(', ')}`);
    }

    // Show instruction preview
    if (parsed.instructions) {
      console.log();
      divider();
      info('Instructions preview:');
      const lines = parsed.instructions.split('\n');
      const preview = lines.slice(0, 10).join('\n');
      console.log(preview);
      if (lines.length > 10) {
        info(`... (${lines.length - 10} more lines)`);
      }
    }
  });

export const skillsCommand = new Command('skills')
  .description('Manage agent skills (search, install, list, info)')
  .addCommand(searchCommand)
  .addCommand(installCommand)
  .addCommand(listCommand)
  .addCommand(infoCommandDef);
