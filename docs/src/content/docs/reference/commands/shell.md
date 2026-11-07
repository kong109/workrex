---
title: "shell"
description: Open a native Rex shell in a remote workspace
---

Opens a shell in a workspace on a [remote server](/guide/remote/). The shell gets
its own session on the node's Rex server, and Rex.app shows that session. It
starts in the workspace directory; callers do not need the SSH address or path.

```bash
workrex shell <name> [--host]
```

## Arguments

`name` is a required remote workspace handle, branch, or `project:handle`.
The command runs on the local machine and resolves the workspace's recorded Rex
placement. A local Rex window does not stand in for the remote session.

## Where the shell runs

- With `isolation: none`, it runs directly on the server with the workspace's
  environment.
- With container isolation, it runs in a new container using the workspace's
  image, mounts, and environment. `--host` explicitly requests a server shell.

SSH carries workspace operations and status; terminal input and display belong
to Rex. The shell's native Rex session is independent of the terminal that ran
`workrex shell`.

## Ending the shell

Exit the shell to end it. The agent and workspace continue running. Closing the
shell's Rex session also ends the shell. Stopping the execution or removing the
workspace closes its ad-hoc shells. Disconnecting Rex.app leaves the server's
sessions running so they can be shown again after reconnecting.

## Errors

- Local worktrees are refused. Use `cd "$(workrex path <name>)"` for a local one.
- An unreachable node fails with the remote's name.
- A workspace that has not started is refused with a hint to run
  `workrex open <name>`.
- Showing the shell in Rex.app needs the node's Rex server to be reachable and
  Remote Control enabled.
