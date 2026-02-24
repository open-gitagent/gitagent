#!/usr/bin/env node

import { Command } from 'commander';
import { initCommand } from './commands/init.js';
import { validateCommand } from './commands/validate.js';
import { infoCommand } from './commands/info.js';
import { exportCommand } from './commands/export.js';
import { importCommand } from './commands/import.js';
import { installCommand } from './commands/install.js';
import { auditCommand } from './commands/audit.js';
import { skillsCommand } from './commands/skills.js';
import { runCommand } from './commands/run.js';
import { lyzrCommand } from './commands/lyzr.js';

const program = new Command();

program
  .name('gitagent')
  .description('A framework-agnostic, git-native standard for defining AI agents')
  .version('0.1.0');

program.addCommand(initCommand);
program.addCommand(validateCommand);
program.addCommand(infoCommand);
program.addCommand(exportCommand);
program.addCommand(importCommand);
program.addCommand(installCommand);
program.addCommand(auditCommand);
program.addCommand(skillsCommand);
program.addCommand(runCommand);
program.addCommand(lyzrCommand);

program.parse();
