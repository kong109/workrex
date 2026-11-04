---
title: "wait"
description: Block until agents reach a target status
---

Blocks until the agents in the given worktrees reach a status. By default it waits until each worktree has an agent that is `done`.

```bash
workrex wait <worktree>... [--status <status>] [--timeout <seconds>] [--any]
```

## Arguments

- `<worktree>...`: One or more worktree names. Use `project:handle` to reach an agent in another project.

## Options

| Flag                  | Description                                                              |
| --------------------- | ------------------------------------------------------------------------ |
| `--status <status>`   | Status to wait for: `working`, `waiting` or `done`. Default: `done`.     |
| `--timeout <seconds>` | Maximum time to wait. Without it, `wait` waits indefinitely.             |
| `--any`               | Return when any one worktree reaches the status, instead of all of them. |

`wait` prints one line to stderr for each worktree as it finishes, for example `user-auth: done (3m 12s)`. A worktree also counts as finished when its agent stops and the worktree no longer exists, which `wait` reports as `merged`. A remote workspace that is removed reports `removed`.

## Exit status

| Status | Meaning                                                                                                                                           |
| ------ | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `0`    | The worktrees reached the status.                                                                                                                 |
| `1`    | The timeout passed, and `wait` names the worktrees it was still waiting for. `wait` also exits with `1` on an error, such as an unknown worktree. |
| `3`    | An agent that was running stopped while its worktree still exists.                                                                                |

`wait` keeps waiting for an agent that has not started yet. Use `--timeout` to bound that.

## Examples

```bash
# Wait for two agents to finish
workrex wait feature-a feature-b

# Confirm that an agent started
workrex wait user-auth --status working --timeout 60

# Continue when the first of several agents finishes
workrex wait task-1 task-2 task-3 --any
```

## See also

- [`status`](/reference/commands/status/) to check status once without blocking
- [Skills](/guide/skills/) for how a coordinator agent uses `wait`
