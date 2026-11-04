---
title: "capture"
description: Print recent terminal output from a running agent
---

Prints the last lines of the agent pane in a worktree, as plain text.

```bash
workrex capture <name> [--lines <n>]
```

## Arguments

- `<name>`: Worktree name. Use `project:handle` to reach an agent in another project.

## Options

| Flag              | Description                               |
| ----------------- | ----------------------------------------- |
| `-n, --lines <n>` | Number of lines to print. Default: `200`. |

`capture` strips ANSI escape codes and trailing blank lines before it keeps the last `<n>` lines.

For a workspace on a [remote server](/guide/remote/), `capture` reads the remote agent's pane.

Under [Rex](/guide/rex/), `capture` reads the block's full scrollback and keeps the last lines.

## Examples

```bash
# Print the last 200 lines of an agent's output
workrex capture user-auth

# Print the last 50 lines
workrex capture user-auth -n 50

# Read an agent in another project
workrex capture myproject:docs-update
```

## See also

- [`send`](/reference/commands/send/) to give the agent more instructions
- [`status`](/reference/commands/status/) to check the agent's status
