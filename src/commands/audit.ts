import { Command } from 'commander';
import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { loadAgentManifest } from '../utils/loader.js';
import { success, error, warn, info, heading, label, divider } from '../utils/format.js';

interface AuditOptions {
  dir: string;
}

export const auditCommand = new Command('audit')
  .description('Generate compliance audit report')
  .option('-d, --dir <dir>', 'Agent directory', '.')
  .action((options: AuditOptions) => {
    const dir = resolve(options.dir);

    let manifest;
    try {
      manifest = loadAgentManifest(dir);
    } catch (e) {
      error((e as Error).message);
      process.exit(1);
    }

    heading('Compliance Audit Report');
    label('Agent', `${manifest.name} v${manifest.version}`);
    label('Date', new Date().toISOString().split('T')[0]);
    divider();

    const c = manifest.compliance;

    if (!c) {
      warn('No compliance configuration found in agent.yaml');
      info('Add a compliance section to enable regulatory audit checks');
      return;
    }

    // Risk Classification
    heading('1. Risk Classification');
    label('Risk Tier', (c.risk_tier ?? 'unspecified').toUpperCase());
    label('Frameworks', c.frameworks?.join(', ') ?? 'none');

    // Supervision (FINRA 3110)
    heading('2. Supervision (FINRA Rule 3110)');
    if (c.supervision) {
      const s = c.supervision;
      auditCheck('Designated supervisor assigned', !!s.designated_supervisor);
      auditCheck('Review cadence defined', !!s.review_cadence);
      auditCheck('Human-in-the-loop configured', !!s.human_in_the_loop && s.human_in_the_loop !== 'none');
      auditCheck('Escalation triggers defined', !!(s.escalation_triggers && s.escalation_triggers.length > 0));
      auditCheck('Override capability enabled', s.override_capability === true);
      auditCheck('Kill switch enabled', s.kill_switch === true);

      if (c.risk_tier === 'high' || c.risk_tier === 'critical') {
        auditCheck(
          'HITL is "always" or "conditional" for high/critical risk',
          s.human_in_the_loop === 'always' || s.human_in_the_loop === 'conditional'
        );
      }
    } else {
      warn('  Supervision section not configured');
    }

    // Recordkeeping (FINRA 4511 / SEC 17a-4)
    heading('3. Recordkeeping (FINRA Rule 4511 / SEC 17a-4)');
    if (c.recordkeeping) {
      const r = c.recordkeeping;
      auditCheck('Audit logging enabled', r.audit_logging === true);
      auditCheck('Log format specified', !!r.log_format);
      auditCheck('Retention period defined', !!r.retention_period);
      auditCheck('Prompt/response logging', r.log_contents?.includes('prompts_and_responses') ?? false);
      auditCheck('Tool call logging', r.log_contents?.includes('tool_calls') ?? false);
      auditCheck('Decision pathway logging', r.log_contents?.includes('decision_pathways') ?? false);
      auditCheck('Model version tracking', r.log_contents?.includes('model_version') ?? false);
      auditCheck('Timestamp logging', r.log_contents?.includes('timestamps') ?? false);
      auditCheck('Immutable logs', r.immutable === true);

      // Check retention meets minimums
      if (r.retention_period) {
        const match = r.retention_period.match(/^(\d+)([ymd])$/);
        if (match) {
          const value = parseInt(match[1]);
          const unit = match[2];
          const yearsEquiv = unit === 'y' ? value : unit === 'm' ? value / 12 : value / 365;
          if (c.frameworks?.includes('finra') && yearsEquiv < 6) {
            warn(`  Retention ${r.retention_period} may be below FINRA 4511 minimum (6 years)`);
          }
          if (c.frameworks?.includes('sec') && yearsEquiv < 3) {
            warn(`  Retention ${r.retention_period} may be below SEC 17a-4 minimum (3 years)`);
          }
        }
      }
    } else {
      warn('  Recordkeeping section not configured');
    }

    // Model Risk Management (SR 11-7)
    heading('4. Model Risk Management (SR 11-7)');
    if (c.model_risk) {
      const m = c.model_risk;
      auditCheck('Model inventory ID assigned', !!m.inventory_id);
      auditCheck('Validation cadence defined', !!m.validation_cadence);
      auditCheck('Validation type specified', !!m.validation_type);
      auditCheck('Conceptual soundness documented', !!m.conceptual_soundness);
      auditCheck('Ongoing monitoring enabled', m.ongoing_monitoring === true);
      auditCheck('Outcomes analysis enabled', m.outcomes_analysis === true);
      auditCheck('Drift detection enabled', m.drift_detection === true);
    } else {
      warn('  Model risk section not configured');
      if (c.frameworks?.includes('federal_reserve')) {
        error('  REQUIRED: Federal Reserve framework requires model_risk section (SR 11-7)');
      }
    }

    // Data Governance
    heading('5. Data Governance (Reg S-P, CFPB)');
    if (c.data_governance) {
      const d = c.data_governance;
      auditCheck('PII handling policy defined', !!d.pii_handling);
      auditCheck('PII handling is restrictive', d.pii_handling !== 'allow');
      auditCheck('Data classification set', !!d.data_classification);
      auditCheck('Consent requirement configured', d.consent_required !== undefined);
      auditCheck('Cross-border assessment done', d.cross_border !== undefined);
      auditCheck('Bias testing enabled', d.bias_testing === true);
      auditCheck('LDA search configured', d.lda_search !== undefined);
    } else {
      warn('  Data governance section not configured');
    }

    // Communications (FINRA 2210)
    heading('6. Communications Compliance (FINRA Rule 2210)');
    if (c.communications) {
      const comm = c.communications;
      auditCheck('Communication type classified', !!comm.type);
      auditCheck('Fair and balanced enforced', comm.fair_balanced === true);
      auditCheck('No misleading enforced', comm.no_misleading === true);
      auditCheck('Pre-review requirement assessed', comm.pre_review_required !== undefined);
      auditCheck('Disclosure requirements assessed', comm.disclosures_required !== undefined);

      if (comm.type === 'retail' && !comm.pre_review_required) {
        warn('  Retail communications typically require principal pre-review (FINRA 2210(b)(1))');
      }
    } else {
      warn('  Communications section not configured');
      if (c.frameworks?.includes('finra')) {
        warn('  Recommended: FINRA framework agents should configure communications section');
      }
    }

    // Third-Party Vendor Management (SR 23-4)
    heading('7. Vendor Management (SR 23-4)');
    if (c.vendor_management) {
      const v = c.vendor_management;
      auditCheck('Due diligence complete', v.due_diligence_complete === true);
      auditCheck('SOC report requirement assessed', v.soc_report_required !== undefined);
      auditCheck('Vendor AI notification required', v.vendor_ai_notification === true);
      auditCheck('Subcontractor assessment done', v.subcontractor_assessment === true);
    } else if (manifest.dependencies && manifest.dependencies.length > 0) {
      warn('  Vendor management section not configured but dependencies exist');
      warn('  Consider adding vendor_management per SR 23-4 requirements');
    } else {
      info('  No vendor dependencies — vendor management not required');
    }

    // Compliance artifacts
    heading('8. Compliance Artifacts');
    auditCheck('compliance/ directory exists', existsSync(join(dir, 'compliance')));
    auditCheck('regulatory-map.yaml exists', existsSync(join(dir, 'compliance', 'regulatory-map.yaml')));
    auditCheck('validation-schedule.yaml exists', existsSync(join(dir, 'compliance', 'validation-schedule.yaml')));
    auditCheck('risk-assessment.md exists', existsSync(join(dir, 'compliance', 'risk-assessment.md')));
    auditCheck('RULES.md exists', existsSync(join(dir, 'RULES.md')));

    // Hooks for audit trail
    heading('9. Audit Hooks');
    const hooksExist = existsSync(join(dir, 'hooks', 'hooks.yaml'));
    auditCheck('hooks/hooks.yaml exists', hooksExist);
    if (hooksExist) {
      const hooksContent = readFileSync(join(dir, 'hooks', 'hooks.yaml'), 'utf-8');
      auditCheck('Compliance hooks configured', hooksContent.includes('compliance: true'));
    }

    divider();
    console.log('');
    info('This audit report is for informational purposes only.');
    info('Consult with legal and compliance teams for definitive assessments.');
    console.log('');
  });

function auditCheck(description: string, passed: boolean): void {
  if (passed) {
    success(`  ${description}`);
  } else {
    warn(`  ${description}`);
  }
}
