---
name: task-management
description: Git-native task tracking with state machine — create, assign, transition, and track tasks where every mutation is a git commit
license: MIT
allowed-tools: gitcron cli read write
metadata:
  author: open-gitagent
  version: "1.0.0"
  category: project-management
---

# Task Management

## Instructions

You can create and manage tasks tracked as YAML files in `.gitcron/tasks/`. Every task mutation (create, state change, reassign) creates a git commit, giving you a full audit trail.

### When to use tasks

- Track work items discovered during agent execution
- Break down complex requests into trackable units
- Assign work to team members or other agents
- Track progress through defined states

### Task states

Default state machine:
```
pending → in_progress → review → done
  ↓          ↓           ↓
cancelled  cancelled   (back to in_progress)
```

Valid transitions:
- `pending` → `in_progress`, `cancelled`
- `in_progress` → `review`, `done`, `cancelled`
- `review` → `in_progress`, `done`
- `done` → (terminal)
- `cancelled` → (terminal)

### Creating tasks

Use the `gitcron` tool with `task-create`:
- Always provide a clear, actionable title
- Set priority based on urgency: `high`, `medium`, `low`
- Assign if the responsible person/agent is known

### Updating tasks

Use `task-update` with the task ID (e.g., TASK-001):
- Only transition to valid next states
- The tool will reject invalid transitions

### Workflow

1. `task-list` to see current tasks and their states
2. `task-create` for new work items
3. `task-update` to progress tasks through states
4. `task-show` for full details including history

### Best practices

- One task per discrete work item
- Use descriptive titles that explain the outcome, not the process
- Set priority based on impact: `high` = blocking/urgent, `medium` = normal, `low` = nice-to-have
- Move tasks to `review` when they need human verification
- Move to `done` only when fully complete

## Output Format

When managing tasks, report:
1. Action taken (created/updated/listed)
2. Task ID and current state
3. What changed and why
