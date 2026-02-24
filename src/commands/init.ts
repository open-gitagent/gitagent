import { Command } from 'commander';
import { mkdirSync, writeFileSync, existsSync, readFileSync, appendFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { success, error, info, heading } from '../utils/format.js';

interface InitOptions {
  template: string;
  dir: string;
}

const MINIMAL_AGENT_YAML = `spec_version: "0.1.0"
name: my-agent
version: 0.1.0
description: A helpful assistant agent
`;

const MINIMAL_SOUL_MD = `# Soul

I am a helpful assistant. I communicate clearly and concisely, focusing on providing accurate and useful information.
`;

const STANDARD_AGENT_YAML = `spec_version: "0.1.0"
name: my-agent
version: 0.1.0
description: A helpful assistant agent
author: ""
license: MIT
model:
  preferred: claude-sonnet-4-5-20250929
  constraints:
    temperature: 0.3
    max_tokens: 4096
skills: []
tools: []
runtime:
  max_turns: 30
  timeout: 120
tags: []
`;

const STANDARD_SOUL_MD = `# Soul

## Core Identity
I am a helpful assistant specializing in [your domain].

## Communication Style
Clear, concise, and professional. I adapt my tone to the context.

## Values & Principles
- Accuracy over speed
- Transparency in reasoning
- Helpfulness without overstepping

## Domain Expertise
- [List your areas of expertise]

## Collaboration Style
I ask clarifying questions when requirements are ambiguous.
`;

const STANDARD_RULES_MD = `# Rules

## Must Always
- Provide accurate, well-sourced information
- Ask clarifying questions when requirements are ambiguous
- Acknowledge limitations and uncertainty

## Must Never
- Make claims without supporting evidence
- Provide harmful or dangerous information
- Ignore safety boundaries

## Output Constraints
- Use clear, structured formatting
- Keep responses focused and relevant

## Interaction Boundaries
- Stay within defined domain expertise
- Escalate appropriately when outside scope
`;

const FULL_AGENT_YAML = `spec_version: "0.1.0"
name: my-agent
version: 0.1.0
description: A production-ready agent with full compliance configuration
author: ""
license: proprietary
model:
  preferred: claude-opus-4-6
  fallback:
    - claude-sonnet-4-5-20250929
  constraints:
    temperature: 0.1
    max_tokens: 8192
skills: []
tools: []
delegation:
  mode: auto
runtime:
  max_turns: 50
  timeout: 300
compliance:
  risk_tier: standard
  frameworks: []
  supervision:
    designated_supervisor: null
    review_cadence: quarterly
    human_in_the_loop: conditional
    escalation_triggers:
      - confidence_below: 0.7
      - error_detected: true
    override_capability: true
    kill_switch: true
  recordkeeping:
    audit_logging: true
    log_format: structured_json
    retention_period: 6y
    log_contents:
      - prompts_and_responses
      - tool_calls
      - decision_pathways
      - model_version
      - timestamps
    immutable: true
  model_risk:
    inventory_id: null
    validation_cadence: annual
    validation_type: full
    conceptual_soundness: null
    ongoing_monitoring: true
    outcomes_analysis: true
    drift_detection: true
    parallel_testing: false
  data_governance:
    pii_handling: redact
    data_classification: confidential
    consent_required: true
    cross_border: false
    bias_testing: true
    lda_search: false
  communications:
    type: correspondence
    pre_review_required: false
    fair_balanced: true
    no_misleading: true
    disclosures_required: false
  vendor_management:
    due_diligence_complete: false
    soc_report_required: false
    vendor_ai_notification: true
    subcontractor_assessment: false
tags: []
metadata: {}
`;

const FULL_RULES_MD = `# Rules

## Must Always
- Provide accurate, well-sourced information
- Log all decisions with reasoning trace
- Escalate to supervisor when confidence is below threshold
- Include confidence levels with assessments

## Must Never
- Make determinations without human review for high-risk decisions
- Store PII in outputs or logs without authorization
- Generate misleading, exaggerated, or promissory statements
- Override human-in-the-loop escalation triggers

## Output Constraints
- Use structured formatting with clear sections
- Include standard disclaimer where required
- Maximum response length per policy

## Interaction Boundaries
- Only process data explicitly provided
- Do not access external systems without authorization
- Scope limited to defined domain

## Safety & Ethics
- Report potential conflicts of interest
- Protect confidential information
- Do not assist in circumventing regulatory requirements

## Regulatory Constraints
- All outputs subject to applicable regulatory framework
- Communications must be fair and balanced
- Audit trail must be maintained for all decisions
`;

const AGENTS_MD = `# Agent

A brief description of this agent for tools that read AGENTS.md (Cursor, Copilot, etc.).

## Capabilities
- [List key capabilities]

## Constraints
- [List key constraints]
`;

const HOOKS_YAML = `hooks:
  on_session_start:
    - script: scripts/on-start.sh
      description: Initialize session context
      timeout: 10
      compliance: false
      fail_open: true
  on_error:
    - script: scripts/on-error.sh
      description: Handle errors and escalate if needed
      timeout: 10
      compliance: false
      fail_open: true
`;

const HOOK_SCRIPT = `#!/usr/bin/env bash
set -euo pipefail
INPUT=$(cat)
echo '{"action": "allow", "modifications": null}'
`;

const MEMORY_YAML = `layers:
  - name: working
    path: MEMORY.md
    max_lines: 200
    format: markdown
  - name: archive
    path: archive/
    format: yaml
    rotation: monthly
update_triggers:
  - on_session_end
  - on_explicit_save
archive_policy:
  max_entries: 1000
  compress_after: 90d
`;

const MEMORY_MD = `# Memory

This file tracks persistent state across sessions. Max 200 lines.
`;

const KNOWLEDGE_INDEX = `documents: []
`;

const SKILL_MD = `---
name: example-skill
description: An example skill
license: MIT
allowed-tools: ""
metadata:
  author: ""
  version: "1.0.0"
  category: general
---

# Example Skill

## Instructions
Describe the skill instructions here.
`;

const REGULATORY_MAP = `mappings: []
`;

const VALIDATION_SCHEDULE = `schedule: []
`;

const RISK_ASSESSMENT = `# Risk Assessment

## Agent: [name]
## Risk Tier: [tier]
## Assessment Date: [date]
## Assessor: [name]

## Risk Tier Justification
[Explain why this risk tier was chosen]

## Applicable Regulatory Frameworks
[List applicable frameworks and rules]

## Risk Categories
[Assess each risk category]

## Mitigation Controls
[List controls and their status]

## Approval
- [ ] Risk team approval
- [ ] Compliance team approval
- [ ] Supervisor approval
`;

function createDir(path: string): void {
  if (!existsSync(path)) {
    mkdirSync(path, { recursive: true });
  }
}

function createFile(path: string, content: string): void {
  writeFileSync(path, content, 'utf-8');
}

export const initCommand = new Command('init')
  .description('Scaffold a new gitagent repository')
  .option('-t, --template <template>', 'Template to use (minimal, standard, full)', 'standard')
  .option('-d, --dir <dir>', 'Target directory', '.')
  .action((options: InitOptions) => {
    const dir = resolve(options.dir);
    const template = options.template;

    if (existsSync(join(dir, 'agent.yaml'))) {
      error('agent.yaml already exists in this directory');
      process.exit(1);
    }

    heading(`Scaffolding ${template} gitagent`);

    if (template === 'minimal') {
      createFile(join(dir, 'agent.yaml'), MINIMAL_AGENT_YAML);
      createFile(join(dir, 'SOUL.md'), MINIMAL_SOUL_MD);
      success('Created agent.yaml');
      success('Created SOUL.md');
    } else if (template === 'standard') {
      createFile(join(dir, 'agent.yaml'), STANDARD_AGENT_YAML);
      createFile(join(dir, 'SOUL.md'), STANDARD_SOUL_MD);
      createFile(join(dir, 'RULES.md'), STANDARD_RULES_MD);
      createFile(join(dir, 'AGENTS.md'), AGENTS_MD);

      createDir(join(dir, 'skills', 'example-skill'));
      createFile(join(dir, 'skills', 'example-skill', 'SKILL.md'), SKILL_MD);
      createDir(join(dir, 'tools'));
      createDir(join(dir, 'knowledge'));
      createFile(join(dir, 'knowledge', 'index.yaml'), KNOWLEDGE_INDEX);

      success('Created agent.yaml');
      success('Created SOUL.md');
      success('Created RULES.md');
      success('Created AGENTS.md');
      success('Created skills/example-skill/SKILL.md');
      success('Created knowledge/index.yaml');
    } else if (template === 'full') {
      createFile(join(dir, 'agent.yaml'), FULL_AGENT_YAML);
      createFile(join(dir, 'SOUL.md'), STANDARD_SOUL_MD);
      createFile(join(dir, 'RULES.md'), FULL_RULES_MD);
      createFile(join(dir, 'AGENTS.md'), AGENTS_MD);

      createDir(join(dir, 'skills', 'example-skill'));
      createFile(join(dir, 'skills', 'example-skill', 'SKILL.md'), SKILL_MD);

      createDir(join(dir, 'tools'));
      createDir(join(dir, 'knowledge'));
      createFile(join(dir, 'knowledge', 'index.yaml'), KNOWLEDGE_INDEX);

      createDir(join(dir, 'memory', 'archive'));
      createFile(join(dir, 'memory', 'MEMORY.md'), MEMORY_MD);
      createFile(join(dir, 'memory', 'memory.yaml'), MEMORY_YAML);

      createDir(join(dir, 'workflows'));

      createDir(join(dir, 'hooks', 'scripts'));
      createFile(join(dir, 'hooks', 'hooks.yaml'), HOOKS_YAML);
      createFile(join(dir, 'hooks', 'scripts', 'on-start.sh'), HOOK_SCRIPT);
      createFile(join(dir, 'hooks', 'scripts', 'on-error.sh'), HOOK_SCRIPT);

      createDir(join(dir, 'examples', 'scenarios'));

      createDir(join(dir, 'agents'));

      createDir(join(dir, 'compliance'));
      createFile(join(dir, 'compliance', 'regulatory-map.yaml'), REGULATORY_MAP);
      createFile(join(dir, 'compliance', 'validation-schedule.yaml'), VALIDATION_SCHEDULE);
      createFile(join(dir, 'compliance', 'risk-assessment.md'), RISK_ASSESSMENT);

      createDir(join(dir, 'config'));
      createFile(join(dir, 'config', 'default.yaml'), 'log_level: info\ncompliance_mode: true\n');

      // Add .gitagent to .gitignore
      const gitignorePath = join(dir, '.gitignore');
      if (existsSync(gitignorePath)) {
        const existing = readFileSync(gitignorePath, 'utf-8');
        if (!existing.includes('.gitagent/')) {
          appendFileSync(gitignorePath, '\n.gitagent/\n');
        }
      } else {
        createFile(gitignorePath, '.gitagent/\n');
      }

      success('Created agent.yaml (with compliance config)');
      success('Created SOUL.md');
      success('Created RULES.md');
      success('Created AGENTS.md');
      success('Created skills/example-skill/SKILL.md');
      success('Created knowledge/index.yaml');
      success('Created memory/MEMORY.md + memory.yaml');
      success('Created hooks/hooks.yaml + scripts');
      success('Created compliance/ (regulatory-map, validation-schedule, risk-assessment)');
      success('Created config/default.yaml');
    } else {
      error(`Unknown template: ${template}. Use minimal, standard, or full.`);
      process.exit(1);
    }

    info(`\nAgent scaffolded at ${dir}`);
    info('Next steps:');
    info('  1. Edit agent.yaml with your agent details');
    info('  2. Write your SOUL.md identity');
    if (template !== 'minimal') {
      info('  3. Run `gitagent validate` to check your configuration');
    }
  });
