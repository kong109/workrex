---
title: "run"
description: Run a command in a split next to a worktree's agent
---

Runs a shell command in a new pane next to the agent in a worktree, in the worktree's directory. By default it streams the command's output and exits with the command's exit status.

```bash
workrex run <name> [options] -- <command>...
```

## Arguments

- `<name>`: Worktree name. Use `project:handle` to reach a worktree in another project.
- `<command>...`: The command and its arguments, after `--`. `run` quotes each argument, so argument boundaries survive.

## Options

| Flag                  | Description                                                                                               |
| --------------------- | --------------------------------------------------------------------------------------------------------- |
| `-b, --background`    | Start the command and return without waiting. Prints the run ID, the pane ID and the artifacts directory. |
| `--keep`              | Keep the run's artifacts (command, stdout, stderr and result) after it finishes.                          |
| `--timeout <seconds>` | Stop waiting after this many seconds and exit with status `124`.                                          |

## What happens

1. Finds the agent pane for the worktree.
2. Splits that pane vertically and gives the new pane 30% of the space.
3. Runs the command in the new pane, in the worktree's directory.
4. Streams the command's stdout and stderr to your terminal, unless you pass `--background`.
5. Exits with the command's exit status, then deletes the artifacts unless you pass `--keep`.

Under [Rex](/guide/rex/), the new pane is a Rex block in the agent's window.

## Examples

```bash
# Run the tests next to an agent and stream the output
workrex run user-auth -- cargo test

# Start a dev server and return at once
workrex run user-auth --background -- npm run dev

# Give up after five minutes
workrex run user-auth --timeout 300 -- make check
```
