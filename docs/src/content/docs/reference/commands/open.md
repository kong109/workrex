---
title: "open"
description: Open or switch to a Rex window for an existing worktree
---

Opens or switches to a multiplexer window for an existing git worktree. If the window already exists, switches to it. If not, creates a new window with the configured pane layout and environment. Accepts multiple names to open several worktrees at once.

```bash
workrex open [name...] [flags]
```

## Arguments

- `[name...]`: One or more worktree names, matching the worktree directory or handle. The Rex target can differ when a worktree uses `--target-name` or `--parent-session`. Optional with `--new` when run from inside a worktree.

## Options

| Flag                       | Description                                                                                                                                                                                                                                                                        |
| -------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `-n, --new`                | Force opening in a new window even if one already exists. Creates a duplicate window with a suffix (e.g., `-2`, `-3`). Useful for having multiple terminal views into the same worktree. The duplicate appears after the caller's Rex window. Cannot be used with session mode.    |
| `--mode <window\|session>` | Override the multiplexer mode for this command. `session` persists the mode change for subsequent opens. `window` converts a session-mode worktree back to window mode. Session mode is the default.                                                                               |
| `-s, --session`            | Shorthand for `--mode session`. Persists the mode change for subsequent opens. Cannot be combined with `--mode`.                                                                                                                                                                   |
| `--target-name <name>`     | Override the workrex-managed Rex target name for this command. In window mode, creates or selects window `<window_prefix><name>`. In session mode, creates or selects session `<window_prefix><name>`. Cannot be used with multiple worktree names.                                |
| `--parent-session <name>`  | Window mode only. Creates the workrex-managed window inside the named Rex session without applying `window_prefix` to that parent session. Cannot be used with session mode or multiple worktree names.                                                                            |
| `--config <path>`          | Use an alternate config file for this invocation. Still merges with global config. Useful for per-command config overrides like `workrex open feat/my-branch --config /path/to/workrex.session.yaml`. Use Workrex configuration paths; see [Configuration](/guide/configuration/). |
| `--run-hooks`              | Re-runs the `post_create` commands (these block window creation).                                                                                                                                                                                                                  |
| `--force-files`            | Re-applies file copy/symlink operations. Useful for restoring a deleted `.env` file.                                                                                                                                                                                               |
| `-p, --prompt <text>`      | Provide an inline prompt for AI agent panes.                                                                                                                                                                                                                                       |
| `-P, --prompt-file <path>` | Provide a path to a file containing the prompt.                                                                                                                                                                                                                                    |
| `-c, --continue`           | Resume the agent's most recent conversation in this worktree. Injects the appropriate flag for the configured agent (e.g., `--continue` for Claude, `--resume` for Gemini).                                                                                                        |
| `-e, --prompt-editor`      | Open your editor to write the prompt interactively.                                                                                                                                                                                                                                |
| `--prompt-file-only`       | Write the prompt file to the worktree without injecting it into agent commands.                                                                                                                                                                                                    |

## What happens

1. Verifies that a worktree with `<name>` exists.
2. If the target exists and `--new` is not set, switches to it.
3. Otherwise, creates the managed Rex session or window. `--target-name` can override its label, and `--parent-session` chooses a known session for window mode.
4. Runs file operations and `post_create` hooks when requested.
5. Sets up the configured terminal blocks.
6. Makes the target active in its Rex session. Rex.app stays where it is.

Window placement uses the caller's Rex IDs or an explicitly chosen parent
session. A repository path is not a session identifier.

## Under Rex

Under [Rex](/guide/rex/), `open` uses the mode the worktree was created or last opened with. A worktree with no stored mode opens in its own Rex session unless you set `mode: window` or pass `--mode window`. `open` makes the window or session active in Rex, but it never moves Rex.app. Open the tab in the app yourself, or press Enter on the worktree's row in [`workrex dashboard`](/reference/commands/dashboard/). `--new` needs window mode.

## Remote workspaces

For a workspace on a [remote server](/guide/remote/), `open` asks Rex.app to
show its recorded session on the node's Rex server. It does not create a local
relay terminal. If the execution ended, `open` starts the workspace again and
uses the agent's resume arguments when available. Showing the target needs the
node's Rex server to be reachable by the app and Remote Control enabled.

## Examples

```bash
# Open or switch to a window for an existing worktree
workrex open user-auth

# Force open a second window for the same worktree (creates user-auth-2)
workrex open user-auth --mode window --new

# Open a new window for the current worktree (run from within the worktree)
workrex open --new

# Open in session mode (converts from window mode if needed)
workrex open user-auth --mode session

# Convert a session-mode worktree back to a window
workrex open user-auth --mode window

# Recreate a closed worktree with a custom window target
workrex open user-auth --target-name review-auth

# Recreate a window-mode worktree inside a named Rex session
workrex open user-auth --parent-session prs --target-name review-auth

# Recreate a worktree as a custom-named session
workrex open user-auth --mode session --target-name review-auth

# Resume the agent's last conversation
workrex open user-auth --continue

# Resume and send a follow-up prompt
workrex open user-auth --continue -p "Continue implementing the login flow"

# Open and re-run dependency installation
workrex open user-auth --run-hooks

# Open and restore configuration files
workrex open user-auth --force-files

# Open multiple worktrees at once
workrex open user-auth api-refactor bugfix-login
```
