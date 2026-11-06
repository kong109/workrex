---
title: "Maintenance and troubleshooting"
description: Upgrade, clean up and retire remote servers, and fix common problems
---

## Upgrading workrex

After you upgrade workrex on the local machine, the next command that reaches a server installs the matching version there. Running agents keep the version they started with.

If you run workrex built from source, the server needs a Linux build of the same source: point [`remotes.<name>.binary`](/guide/configuration/#remotes) at it.

## Cleaning up a server

```bash
workrex remote gc devbox --dry-run   # list only
workrex remote gc devbox             # list, ask, remove
workrex remote gc devbox --force     # remove without asking, including unsaved work
```

`remote gc` lists what is on the server that no workspace on this machine accounts for, and removes it after you confirm: workspaces the local machine has no record of (for example after `remove --force` while the server was unreachable), leftover worktrees, sessions and containers, and workrex versions no agent uses. It also forgets local records of workspaces the server no longer has.

A workspace with uncommitted changes or commits beyond its base is listed, but only removed with `--force`. Workspaces in the middle of starting, a merge or a pull are left alone. Without a terminal, nothing is removed unless you pass `--force`.

:::caution[One machine per `path`]
`gc` treats every workspace this machine doesn't know as unused. If two machines use the same server, give each its own [`path`](/guide/configuration/#remotes).
:::

## Retiring a server

1. Merge, pull or remove the workspaces you want to keep.
2. Run `workrex remote gc devbox --force`. This removes work that exists only on the server.
3. Delete the server's `path` directory, `~/.workrex-node` by default. workrex has no uninstall command.
4. Remove the server from `remotes`, and from any `placement` that names it.

If the server is already gone, `workrex remove --force <handle>` forgets its workspaces on the local machine.

## Common problems

**`ssh` fails or asks for a password.** Run `ssh <host>` yourself first. workrex never prompts, so the key must be in your SSH agent or have no passphrase, and the host key must already be in `known_hosts`. `remote doctor` names the failing step: authentication, host key, DNS, refused connection or timeout.

**Docker permission denied after adding the user to the `docker` group.** workrex reuses an SSH connection for up to ten minutes, which keeps the old group membership. Doctor's fix includes the command that closes the connection.

**Agents stop when you log out of the server.** Enable linger: `sudo loginctl enable-linger <user>`.

**An agent asks you to log in.** Run `workrex remote login <remote> <agent>`. `remote doctor` shows which agents have credentials on the server.

**A login fails because the agent is missing.** On the server directly, install the agent there. In Docker mode with a `sandbox.image` override, the image must contain the agent.

**No status icons for a remote agent.** Run `workrex remote setup <remote>` again and check doctor's `hooks` row. For agents on the server directly, OpenCode's hooks are installed once its CLI is on the login `PATH`. In Docker mode, they need the OpenCode sandbox image on the server, so run `setup` again after its first agent or login there.

**Codex asks to review hooks on the server.** Codex outside a sandbox asks once to trust workrex's status hooks. Attach to the agent and trust them; Codex asks again only when the hooks change.

**`workrex: command not found` in a remote pane.** Expected in a Docker-mode shell pane: run the command on the local machine. The agent's container has `workrex`. On the server directly, `workrex` is on every pane's `PATH`. In a shell you opened on the server yourself, such as an SSH login or a Rex window, server mode puts `workrex` on the `PATH` through `~/.local/bin/workrex`. If it's missing there, run `workrex remote setup <remote>` on your machine and check doctor's `path` row, which also says when `~/.local/bin` isn't on the login `PATH`. That `workrex` doesn't know your remote workspaces: run commands for them on your own machine with the workspace's handle. See [running commands](/guide/remote/usage/).

**`rm` or `merge` in a remote pane says the request was withdrawn.** The local machine isn't connected, or runs an older workrex. Run the command on the local machine instead.

**A merge requested from a remote pane failed.** The pane prints a `workrex remote logs` command. Run it on the local machine to see the full output.

**Protocol mismatch.** The server's workrex doesn't match your local one. This happens with builds from source; point `binary` at a Linux build of the same source.

**Something is left over on the server.** Run `workrex remote gc <remote> --dry-run`.

## Known limitations

- Remote terminal sessions belong to the node's Rex server. Rex.app must be able to reach it, and cross-session jumps need Remote Control.
- Servers run Linux and use Docker for sandboxing; Podman, Lima and Apple Container aren't available there.
- The dashboard can't create remote agents; use `workrex add --remote`.
- Agents on a server can't push, read the local machine's clipboard or run host commands.
- `rm` and `merge` requested from a container run without hooks.
- Ignored files, submodule contents and agent conversations don't come home with `merge` or `remote pull`.
- The environment snapshot is taken once per workspace.
- Imported credentials can drift apart from the local machine's copy. `omp` logins can't be imported, and doctor can't tell whether `omp` is logged in.
- The dashboard's diff view of a remote agent is fetched from the server and cached; patch mode is local-only.
- Operation logs aren't deleted automatically.
- A server's `path` is meant for one machine.
