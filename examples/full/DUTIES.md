# Duties

System-wide segregation of duties policy for the compliance-analyst agent system.

## Roles

| Role | Agent | Permissions | Description |
|------|-------|-------------|-------------|
| Analyst | compliance-analyst | create, submit | Performs regulatory analysis, generates findings and reports |
| Reviewer | fact-checker | review, approve, reject | Reviews analysis for accuracy, verifies claims against authoritative sources |
| Auditor | (unassigned) | audit, report | Audits completed reviews and maintains the compliance trail |

## Conflict Matrix

No single agent may hold both roles in any pair:

- **Analyst <-> Reviewer** — The agent that produces findings cannot approve them
- **Analyst <-> Auditor** — The agent that produces findings cannot audit them
- **Reviewer <-> Auditor** — The agent that approves findings cannot audit the approval

## Handoff Workflows

### Regulatory Filing
1. **Analyst** creates the filing draft and submits for review
2. **Reviewer** verifies accuracy against authoritative sources, approves or rejects
3. Approval required at each step before proceeding

### Customer Communication
1. **Analyst** drafts the communication
2. **Reviewer** checks for FINRA 2210 compliance (fair, balanced, no misleading statements)
3. Approval required before any communication is sent

## Isolation Policy

- **State isolation: full** — Each agent operates with its own memory and state. No agent may read or modify another agent's working memory.
- **Credential segregation: separate** — Each role has its own credential scope. The analyst's data access credentials are distinct from the reviewer's.

## Enforcement

Enforcement mode is **strict**. Any SOD violation (e.g., assigning conflicting roles to the same agent) will fail validation and block deployment.
