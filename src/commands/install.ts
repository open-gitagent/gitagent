import { Command } from 'commander';
import { existsSync, mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { execSync } from 'node:child_process';
import { loadAgentManifest } from '../utils/loader.js';
import { success, error, info, heading, divider, warn } from '../utils/format.js';

interface InstallOptions {
  dir: string;
}

export const installCommand = new Command('install')
  .description('Resolve and install agent dependencies')
  .option('-d, --dir <dir>', 'Agent directory', '.')
  .action((options: InstallOptions) => {
    const dir = resolve(options.dir);

    let manifest;
    try {
      manifest = loadAgentManifest(dir);
    } catch (e) {
      error((e as Error).message);
      process.exit(1);
    }

    heading('Installing dependencies');

    if (!manifest.dependencies || manifest.dependencies.length === 0) {
      info('No dependencies to install');
      return;
    }

    const depsDir = join(dir, '.gitagent', 'deps');
    mkdirSync(depsDir, { recursive: true });

    for (const dep of manifest.dependencies) {
      divider();
      info(`Installing ${dep.name} from ${dep.source}`);

      const targetDir = dep.mount
        ? join(dir, dep.mount)
        : join(depsDir, dep.name);

      if (existsSync(targetDir)) {
        warn(`${dep.name} already exists at ${targetDir}, skipping`);
        continue;
      }

      // Check if source is a local path
      if (existsSync(resolve(dir, dep.source))) {
        // Local dependency — symlink or copy
        const sourcePath = resolve(dir, dep.source);
        try {
          mkdirSync(join(targetDir, '..'), { recursive: true });
          execSync(`cp -r "${sourcePath}" "${targetDir}"`, { stdio: 'pipe' });
          success(`Installed ${dep.name} (local)`);
        } catch (e) {
          error(`Failed to install ${dep.name}: ${(e as Error).message}`);
        }
      } else if (dep.source.includes('github.com') || dep.source.endsWith('.git')) {
        // Git dependency
        try {
          const versionFlag = dep.version ? `--branch ${dep.version.replace('^', '')}` : '';
          mkdirSync(join(targetDir, '..'), { recursive: true });
          execSync(`git clone --depth 1 ${versionFlag} "${dep.source}" "${targetDir}" 2>&1`, {
            stdio: 'pipe',
            timeout: 60000,
          });
          success(`Installed ${dep.name} (git)`);
        } catch (e) {
          error(`Failed to clone ${dep.name}: ${(e as Error).message}`);
        }
      } else {
        warn(`Unknown source type for ${dep.name}: ${dep.source}`);
      }

      // Validate installed dependency
      const depAgentYaml = join(targetDir, 'agent.yaml');
      if (existsSync(depAgentYaml)) {
        success(`${dep.name} is a valid gitagent`);
      } else {
        warn(`${dep.name} does not contain agent.yaml — may not be a gitagent`);
      }
    }

    divider();
    success('Dependencies installed');
  });
