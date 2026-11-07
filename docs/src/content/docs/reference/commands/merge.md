---
title: "merge"
description: Merge a branch and clean up the worktree, window, and local branch
---

Merges a branch into a target branch (main by default) and automatically cleans up all associated resources (worktree, Rex window, and local branch).

```bash
workrex merge [branch-name] [flags]
```

:::tip[When to use `merge` vs `remove`]
`workrex merge` performs the git merge locally. Use it when you want to merge directly without a pull request.

If your workflow uses pull requests, the merge happens on the remote after review. In that case, use [`workrex remove`](/reference/commands/remove/) to clean up the worktree after your PR is merged.
:::

## Arguments

- `[branch-name]`: Optional name of the branch to merge. If omitted, automatically detects the current branch from the worktree you're in.

## Options

| Flag                   | Description                                                                                                                                                                                                                                              |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--into <branch>`      | Merge into the specified branch instead of main. Useful for stacked PRs, git-flow workflows, or merging subtasks into a parent feature branch. If the target branch has its own worktree, the merge happens there; otherwise, the main worktree is used. |
| `--ignore-uncommitted` | Merge even when the worktree has unstaged, untracked or staged changes. `merge` does not commit the staged changes.                                                                                                                                      |
| `--keep, -k`           | Keep the worktree, window, and branch after merging (skip cleanup). Useful when you want to verify the merge before cleaning up.                                                                                                                         |
| `--cleanup`            | Clean up after merging, overriding `merge_keep: true`.                                                                                                                                                                                                   |
| `-n, --no-verify`      | Skip the `pre_merge` hooks.                                                                                                                                                                                                                              |
| `--no-hooks`           | Skip all hooks, both `pre_merge` and `pre_remove`.                                                                                                                                                                                                       |
| `--notification`       | Show a system notification on successful merge. Useful when delegating merge to an AI agent and you want to be notified when it completes.                                                                                                               |
| `--rebase`             | Rebase the feature branch onto the target before merging (creates a linear history via fast-forward merge). If conflicts occur, you'll need to resolve them manually and run `git rebase --continue`.                                                    |
| `--squash`             | Squash all commits from the feature branch into a single commit on the target. You'll be prompted to provide a commit message in your editor.                                                                                                            |

## Merge strategies

By default, `workrex merge` performs a standard merge commit (configurable via `merge_strategy`). You can override the configured behavior with these mutually exclusive flags:

- `--rebase`: Rebase the feature branch onto the target before merging (creates a linear history via fast-forward merge). If conflicts occur, you'll need to resolve them manually in the worktree and run `git rebase --continue`.
- `--squash`: Squash all commits from the feature branch into a single commit on the target. You'll be prompted to provide a commit message in your editor.

If you don't want to have merge commits in your main branch, use the `rebase` merge strategy, which does `--rebase` by default.

```yaml
# ~/.config/workrex/config.yaml
merge_strategy: rebase
```

The config path keeps its workrex name so existing workrex configs work. See [Configuration](/guide/configuration/).

To keep the worktree, window, and branch after every merge unless overridden, set:

```yaml
merge_keep: true
```

Use `workrex merge --cleanup` to clean up for a single merge when this default is enabled.

## What happens

1. Determines which branch to merge (specified branch or current branch if omitted)
2. Determines the target branch (`--into` or main branch from config)
3. Checks for uncommitted changes (errors if found, unless `--ignore-uncommitted` is used)
4. Commits staged changes if present (unless `--ignore-uncommitted` is used)
5. Merges your branch into the target using the selected strategy (default: merge commit)
6. Deletes the Rex window unless keep behavior is enabled via `--keep` or `merge_keep: true`
7. Removes the worktree unless keep behavior is enabled via `--keep` or `merge_keep: true`
8. Deletes the local branch unless keep behavior is enabled via `--keep` or `merge_keep: true`

## Remote workspaces

For a workspace on a [remote server](/guide/remote/), run `workrex merge <handle>` from the project on your machine. workrex fetches the branch and the uncommitted work into a local copy, then merges it as usual; all options apply. The remote agent keeps running until the merge succeeds, and its worktree, container and window are then removed. If the remote workspace changed after the fetch, it is kept and the merge still lands.

See [merging remote workspaces](/guide/remote/git/#merging) for preparing the merge worktree, uncommitted work, conflicts and `--keep`. In a remote workspace's pane, or from its agent, `workrex merge` asks the local machine to merge it; see [remove or merge from a remote pane](/guide/remote/usage/#remove-or-merge-from-a-remote-pane).

## Under Rex

When `merge` closes a Rex session that Rex.app shows, the app moves to the first of these sessions that exists: the managed session of the merge target, then [`default_session`](/guide/session-mode/#returning-to-a-default-session), then the session that `add` or `open` ran in when it created the worktree's session. Without a destination, the app lands on the first session. An app that shows another session stays where it is. Closing a session does not need Rex Remote Control. See [Rex session mode](/guide/rex/#session-mode).

## Typical workflow

When you're done working in a worktree, simply run `workrex merge` from within that worktree's Rex window. The command will automatically detect which branch you're on, merge it into main, and close the current window as part of cleanup.

## Examples

```bash
# Merge branch into main (default: merge commit)
workrex merge user-auth

# Merge the current worktree you're in
# (run this from within the worktree's Rex window)
workrex merge

# Rebase onto main before merging for a linear history
workrex merge user-auth --rebase

# Squash all commits into a single commit
workrex merge user-auth --squash

# Merge but keep the worktree/window/branch to verify before cleanup
workrex merge user-auth --keep
# ... verify the merge in main ...
workrex remove user-auth  # clean up later when ready

# Merge into a different branch (stacked PRs)
workrex merge feature/subtask --into feature/parent
```
