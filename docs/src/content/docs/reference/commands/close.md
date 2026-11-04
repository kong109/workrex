---
title: "close"
description: Close a tmux window without removing the worktree or branch
---

Closes the multiplexer window for a worktree, and keeps the worktree and branch. Use it to close a window you plan to return to later.

```bash
workrex close [name]
```

## Arguments

- `[name]`: Optional worktree name (the directory name). Defaults to current directory if omitted.

## Examples

```bash
# Close the window for a specific worktree
workrex close user-auth

# Close the current worktree's window (run from within the worktree)
workrex close
```

## Under Rex

When `close` closes a Rex session that Rex.app shows, the app moves to the first of these sessions that exists: [`default_session`](/guide/session-mode/#returning-to-a-default-session), then the session that `add` or `open` ran in when it created the worktree's session. Without a destination, the app lands on the first session. An app that shows another session stays where it is. Closing a session does not need Rex Remote Control. See [Rex session mode](/guide/rex/#session-mode).

To reopen the window later, use [`workrex open`](/reference/commands/open/).

For a workspace on a [remote server](/guide/remote/), `close` closes only the local window: the agent keeps running on the server. Run it with no name inside the workspace's window, or name the workspace (`workrex close <handle>`). In a pane on the server, `close` is refused, because the window is on the local machine.

:::tip
You can also use tmux's native kill-window command (default: `prefix + &`) to close a worktree's window with the same effect. For worktrees created with `--session`, this closes the entire session.
:::
