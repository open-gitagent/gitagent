---
name: reminders
description: Set recurring or one-shot reminders that create GitHub issues on schedule — for compliance deadlines, reviews, follow-ups, and check-ins
license: MIT
allowed-tools: gitcron cli read write
metadata:
  author: open-gitagent
  version: "1.0.0"
  category: automation
---

# Reminders

## Instructions

You can create reminders that automatically create GitHub issues on a schedule or at a specific time. Reminders are defined in `cron.yaml` and compiled to GitHub Actions workflows.

### Reminder types

**Recurring** — Fires repeatedly on a cron schedule:
```yaml
- name: weekly-standup
  type: recurring
  cron: "0 9 * * 1"
  action:
    type: issue
    title: "Weekly Standup Notes"
    body: "Time for weekly standup. Update your status."
    labels: [meeting, reminder]
    assignees: [team-lead]
```

**One-shot** — Fires once at a specific time:
```yaml
- name: deadline-reminder
  type: one-shot
  at: "2026-04-01T09:00:00Z"
  action:
    type: issue
    title: "Q1 Deadline: Final Review"
```

### When to create reminders

- Compliance deadlines (quarterly reviews, annual audits)
- Follow-ups after deployments or incidents
- Recurring meetings or check-ins
- One-time deadlines or milestones
- Periodic health checks or reviews

### Common cron patterns for reminders

- `0 9 * * 1` — Every Monday at 9 AM (weekly standup)
- `0 9 1 * *` — First of month at 9 AM (monthly review)
- `0 9 1 */3 *` — Quarterly (every 3 months on the 1st)
- `0 9 1 1 *` — Annual (January 1st)
- `0 9 * * 1-5` — Every weekday at 9 AM

### Managing reminders

1. `remind-create` — Add a new recurring reminder
2. `remind-list` — See all reminders
3. `remind-fire` — Manually trigger a reminder now
4. `remind-pause` / `remind-resume` — Temporarily disable/enable
5. After changes, run `gitcron generate` to update workflows

### Best practices

- Use descriptive names in kebab-case: `quarterly-model-review`, not `qmr`
- Include actionable body text explaining what to do
- Add appropriate labels for filtering
- Assign to the responsible team/person
- For compliance reminders, reference the regulation (e.g., "per SR 11-7")

## Output Format

When creating a reminder, confirm:
1. Reminder name and schedule (human-readable)
2. What issue will be created (title, labels, assignees)
3. Next steps: `gitcron generate` and push
