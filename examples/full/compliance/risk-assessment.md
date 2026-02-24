# Risk Assessment

## Agent: compliance-analyst v1.0.0
## Risk Tier: HIGH
## Assessment Date: [To be completed]
## Assessor: [To be completed]

## Risk Tier Justification

This agent is classified as **HIGH** risk because:

1. **Regulatory domain** — Operates in FINRA/SEC/Fed regulated environment
2. **Decision influence** — Analysis outputs influence compliance decisions
3. **Data sensitivity** — May process confidential and restricted financial data
4. **Customer impact** — Findings may affect customer-facing communications and disclosures

## Applicable Regulatory Frameworks

| Framework | Applicability | Key Rules |
|-----------|--------------|-----------|
| FINRA | Direct | 2210, 3110, 3120, 4511 |
| SEC | Direct | Reg BI, Reg S-P, 17a-4 |
| Federal Reserve | Direct | SR 11-7, SR 23-4 |
| CFPB | Conditional | Circular 2022-03 (when credit decisions involved) |

## Risk Categories

### Model Risk (SR 11-7)
- **Inherent Risk:** HIGH — Agent produces regulatory interpretations
- **Residual Risk:** MEDIUM — Mitigated by human-in-the-loop always-on
- **Controls:** Quarterly validation, ongoing monitoring, outcomes analysis

### Supervisory Risk (FINRA 3110)
- **Inherent Risk:** HIGH — Agent outputs could reach customers
- **Residual Risk:** LOW — All outputs require principal pre-review
- **Controls:** Designated supervisor, escalation triggers, kill switch

### Data Risk (Reg S-P)
- **Inherent Risk:** HIGH — Access to confidential data
- **Residual Risk:** MEDIUM — PII redaction enforced, restricted classification
- **Controls:** PII handling: redact, consent required, no cross-border

### Communications Risk (FINRA 2210)
- **Inherent Risk:** MEDIUM — Generates analysis that may inform communications
- **Residual Risk:** LOW — Pre-review required, fair/balanced enforced
- **Controls:** Communications compliance hooks, weekly sampling review

## Mitigation Controls Summary

| Control | Status | Owner |
|---------|--------|-------|
| Human-in-the-loop (always) | Active | designated-supervisor |
| Audit logging (immutable) | Active | records-management |
| PII redaction | Active | data-governance |
| Principal pre-review | Active | compliance-team |
| Kill switch | Active | designated-supervisor |
| Quarterly SR 11-7 validation | Scheduled | model-risk-team |
| Weekly communications sampling | Scheduled | compliance-team |
| Annual vendor review | Scheduled | vendor-management |

## Approval

- [ ] Model Risk Team approval
- [ ] Compliance Team approval
- [ ] Designated Supervisor approval
- [ ] Board/Senior Management acknowledgment
