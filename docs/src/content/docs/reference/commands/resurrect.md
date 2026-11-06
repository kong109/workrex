---
title: "resurrect"
description: Restore worktree windows after a Rex server or computer crash
---

Restores worktree windows after a Rex server or computer crash. Uses persisted agent state files to detect which worktrees had active agents before the crash, then reopens them with each agent's resume arguments.

```bash
workrex resurrect [--dry-run]
```

## Options

- `--dry-run`: Show what would be restored without actually doing it.

## How it works

1. Reads agent state files from `~/.local/state/workrex/agents/`
2. Filters to the current multiplexer backend and instance
3. Matches each state file's working directory to a git worktree in the current repo
4. Skips worktrees that are already open, no longer exist, or are the main worktree
5. Opens each matched worktree with the resume arguments for the remembered agent
6. Uses agent-specific behavior, such as Codex's resume subcommand with its last-session flag
7. Cleans up consumed stale state files

Agent state from known earlier multiplexer boots is compacted to one recovery
candidate per exact working directory. Unrestored worktrees remain recoverable
across repeated restarts without retaining duplicate records for every boot.
Current-boot records, records without a reliable boot identity, malformed files,
and state owned by another multiplexer instance or backend are preserved. Each
instance compacts its own history when workrex observes that instance with a
reliable boot identity.

Configuration and state live under Workrex-owned paths. See [Configuration](/guide/configuration/).

## Remote workspaces

Agents on [remote servers](/guide/remote/) kept running while the local Rex server was down, so their native Rex sessions remain on the server. Use `workrex open <handle>` to show one again in Rex.app. A remote agent whose session ended, for example because the remote host rebooted, is skipped: run `workrex open <handle>` to start it again.

## Examples

```bash
# Preview what would be restored
workrex resurrect --dry-run

# Restore all worktrees that had agents running
workrex resurrect
```

## Example output

```
  continue-flag        -> restoring
  dashboard-fix        -> skipping (already open)
  auth-refactor        -> restoring
  (2 state file(s) from other projects ignored)

✓ Restored 2 worktree(s): continue-flag, auth-refactor
```
