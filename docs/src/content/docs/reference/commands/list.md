---
title: "list"
description: List all git worktrees with their agent, window, and merge status
---

Lists git worktrees with their agent status, multiplexer window status, and merge status. Alias: `ls`

```bash
workrex list [options] [worktree-or-branch...]
```

## Arguments

| Argument             | Description                                                                                     |
| -------------------- | ----------------------------------------------------------------------------------------------- |
| `worktree-or-branch` | Filter by worktree handle (directory name) or branch name. Multiple values supported. Optional. |

## Options

| Flag     | Description                                                                                                                                                                                                                                          |
| -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--all`  | Include worktrees from every repository represented by a tracked agent in the current multiplexer instance. Repositories without tracked agents cannot be discovered.                                                                                |
| `--pr`   | Show GitHub PR status for each worktree. Requires the `gh` CLI to be installed and authenticated. Note that it shows pull requests' statuses with [Nerd Font](https://www.nerdfonts.com/) icons, which requires Nerd Font compatible font installed. |
| `--json` | Output as JSON. Produces a JSON array with `project`, `project_path`, `handle`, `branch`, `path`, `is_main`, `mode`, `has_uncommitted_changes`, `is_open`, `agent_statuses`, and `created_at`.                                                       |

## Examples

```bash
# List all worktrees
workrex list

# List with PR status
workrex list --pr

# Output tracked projects as one JSON response
workrex list --all --json

# Filter to a specific worktree
workrex list my-feature

# Filter to multiple worktrees
workrex list feature-auth feature-api
```

## Remote workspaces

Workspaces on [remote servers](/guide/remote/) are listed alongside local worktrees. Their path names the remote instead of a local directory, with a note while a workspace is still being set up, has a merge or cleanup in progress, or its remote can't be reached. AGENT and UNMERGED come from the remote's last known state, and AGENT says when the agent's session ended, for example `exited (host rebooted)`. In JSON output, they carry a `remote` field with the remote's name, `path` is the worktree's path on the remote, and `agent_statuses` and `has_uncommitted_changes` describe the remote worktree. Local worktrees have no `remote` field.

Remote `is_open` comes from a bounded read of the recorded Rex server, session, and terminal blocks. It is `true` while a configured block lives and `false` when those targets have closed. If the native target cannot be read, `is_open` is `null` and MUX shows `?`. Agent status and connection notes still come from the node's last observation.

## Example output

```
BRANCH      AGE  AGENT  MUX  UNMERGED  PATH
main        -    -      -    -         ~/project
user-auth   2h   🤖     ✓    -         ~/project__worktrees/user-auth
bug-fix     3d   ✅     ✓    ●         ~/project__worktrees/bug-fix
api-work    1w   -      ✓    -         ~/project__worktrees/api-work
```

## Key

- AGE column shows how old the worktree is, based on directory creation time. Uses compact units: `m`, `h`, `d`, `w`, `mo`, `y`. Shows `-` for the main worktree or when creation time is unavailable.
- AGENT column shows the current agent status using [status icons](/guide/status-tracking/):
  - `🤖` = agent is working
  - `💬` = agent is waiting for user input
  - `✅` = agent finished
  - When multiple agents run in one worktree, shows a count (e.g., `2🤖 1✅`)
  - When stdout is piped (e.g., by a script or agent), text labels are used instead: `working`, `waiting`, `done`
- `✓` in MUX column = multiplexer window exists for this worktree
- `?` in MUX column = the remote's native Rex target could not be checked
- `●` in UNMERGED column = branch has commits not merged into main
- `-` = not applicable
