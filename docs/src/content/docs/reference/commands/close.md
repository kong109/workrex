---
title: "close"
description: Close a Rex window without removing the worktree or branch
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

For a workspace on a [remote server](/guide/remote/), `close` is refused because
there is no local relay window to detach. Switch to another session in Rex.app
to leave the remote agent running. Use `workrex remove <handle>` when you intend
to stop and remove the remote workspace.
