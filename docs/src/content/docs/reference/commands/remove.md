---
title: "remove"
description: Remove worktrees, tmux windows, and branches without merging
---

Removes worktrees, tmux windows, and branches without merging (unless you keep the branches). Useful for abandoning work or cleaning up experimental branches. Supports removing multiple worktrees in a single command. Alias: `rm`

```bash
workrex remove [name]... [flags]
```

## Arguments

- `[name]...`: One or more worktree names (the directory names). Defaults to current directory name if omitted.

## Options

| Flag                | Description                                                                                                                                                                      |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--all`             | Remove all worktrees at once (except the main worktree). Prompts for confirmation unless `--force` is used. Safely skips worktrees with uncommitted changes or unmerged commits. |
| `--gone`            | Remove worktrees whose upstream remote branch has been deleted (e.g., after a PR is merged on GitHub). Automatically runs `git fetch --prune` first.                             |
| `--force, -f`       | Skip confirmation prompt and ignore uncommitted changes.                                                                                                                         |
| `--keep-branch, -k` | Remove only the worktree and tmux window while keeping the local branch.                                                                                                         |

## Examples

```bash
# Remove the current worktree (run from within the worktree)
workrex remove

# Remove a specific worktree with confirmation if unmerged
workrex remove experiment

# Remove multiple worktrees at once
workrex rm feature-a feature-b feature-c

# Remove multiple worktrees with force (no confirmation)
workrex rm -f old-work stale-branch

# Use the alias
workrex rm old-work

# Remove worktree/window but keep the branch
workrex remove --keep-branch experiment

# Force remove without prompts
workrex rm -f experiment

# Remove worktrees whose remote branches were deleted (e.g., after PR merge)
workrex rm --gone

# Force remove all gone worktrees (no confirmation)
workrex rm --gone -f

# Remove all worktrees at once
workrex rm --all
```

## Remote workspaces

Pass the handle of a workspace on a [remote server](/guide/remote/) to remove its worktree, agent, container and window from the remote. Uncommitted changes on the remote refuse the removal without `--force`, and commits its base doesn't have ask for confirmation, as with a local worktree. `--keep-branch` fetches the branch to your machine before the remote copy goes. `pre_remove` hooks run on the remote.

If the remote is unreachable or no longer configured, `--force` forgets the workspace on this machine only, leaving whatever exists on the remote for [`workrex remote gc`](/reference/commands/remote/#remote-gc). `--all` and `--gone` only consider local worktrees.

In a remote workspace's pane, or from its agent, `workrex rm` asks the local machine to remove that workspace. See [remove or merge from a remote pane](/guide/remote/usage/#remove-or-merge-from-a-remote-pane).

## Under Rex

When `remove` closes a Rex session that Rex.app shows, the app moves to the first of these sessions that exists: the managed session of the main branch, then [`default_session`](/guide/session-mode/#returning-to-a-default-session), then the session that `add` or `open` ran in when it created the worktree's session. Without a destination, the app lands on the first session. An app that shows another session stays where it is. Closing a session does not need Rex Remote Control. See [Rex session mode](/guide/rex/#session-mode).

## Deferred filesystem cleanup

When removing a worktree from its own terminal target, filesystem cleanup runs
in a detached worker after that target closes. Surviving background processes
can still write into the renamed `.workmux_trash_*` directory. Rust cleanup
continues deleting unrelated files and directories after an individual failure,
using descriptor-relative operations without following symlink targets. workrex retries
`DirectoryNotEmpty` errors with backoff for a five-second retry window; an
individual recursive deletion can take longer. It does not kill those processes.

Before renaming the worktree, workrex saves a pending filesystem-cleanup record
under `$XDG_STATE_HOME/workmux/pending-cleanup/` (by default,
`~/.local/state/workmux/pending-cleanup/`). The record includes the original and
quarantine paths and the captured directory's device and inode. It is cleared
only after filesystem deletion succeeds.

If deletion fails, the record remains and the worker writes the error and a
bounded snapshot of remaining entries, along with the paths and errors from
failed deletion operations, to `workmux.log` in the same state directory.
Records also survive failures in intervening Git operations. They are diagnostic
recovery records, not an automatic cleanup queue: inspect them and stop any
remaining writers before manually recovering the exact quarantined directory.
Do not replay branch deletion from a record, since Git cleanup may already have
completed and the original branch or worktree path may have been reused.

Config and state paths keep their workmux names so existing workmux configs work. See [Configuration](/guide/configuration/).
