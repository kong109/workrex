---
title: "remote"
description: Set up, check and manage SSH remotes that run agents
---

Commands for [remote servers](/guide/remote/): SSH-reachable Linux machines that run individual agents. Remotes are defined under `remotes:` in the global config (see [configuration](/guide/configuration/#remotes)). Create a remote agent with [`workrex add --remote`](/reference/commands/add/).

## remote

List the configured remotes.

```bash
workrex remote
```

Shows each remote's name, its SSH host and its status: reachable or not, whether the node is set up, and the workrex version and protocol it runs. The remote that `placement` selects is marked as the default.

## remote setup

Install the node on a remote.

```bash
workrex remote setup <name>
```

**Arguments:**

- `<name>` - Remote name from `remotes:` in the global config

Uploads the workrex binary for this workrex version into the remote's node root (the release, downloaded and checksum-verified on the local machine, or the configured `binary`), and installs workrex's status hooks for the supported agents in the remote user's agent directories. OpenCode's hooks are installed once the OpenCode sandbox image is on the remote. With `isolation: none`, it also points `~/.local/bin/workrex` on the remote at the installed binary, so shells and scripts on the remote find `workrex`. It creates that file when the path is free and replaces it only when `setup` wrote it, and prints a hint when the remote's login shell doesn't run it. Running it again only installs what is missing. An interrupted install is recovered by the next run.

Commands that need the node, such as `add --remote` and `remote login`, install it automatically, so `setup` is mostly useful as a first check, and after you make OpenCode's sandbox image available on the remote (its hooks depend on the OpenCode version in that image).

## remote doctor

Check that a remote can run agents.

```bash
workrex remote doctor <name>
```

**Arguments:**

- `<name>` - Remote name from `remotes:` in the global config

Prints one row per check, with a fix for each failing row:

| Check       | What it checks                                                                                            |
| ----------- | --------------------------------------------------------------------------------------------------------- |
| `ssh`       | The SSH connection works without a prompt                                                                 |
| `arch`      | The remote runs Linux on a supported architecture                                                         |
| `node`      | The node binary is installed and speaks this workrex's protocol                                           |
| `git`       | Git is installed                                                                                          |
| `rex`       | Rex is installed and its server is reachable                                                              |
| `linger`    | systemd linger is enabled, so agents survive logout                                                       |
| `agents`    | Resolved mode and deciding setting; in host mode, default CLI required and other configured CLIs optional |
| `container` | Docker works for the SSH user (Docker mode only)                                                          |
| `hooks`     | workrex's status hooks are installed for each agent                                                       |
| `path`      | `~/.local/bin/workrex` runs this version's node binary, which exists, in login shells (server mode only)  |
| logins      | One row per configured agent, with the credential files found on the remote                               |
| `image`     | The sandbox image of the default agent can be pulled (it is pulled as part of the check)                  |

Uses the current directory's effective config, like `add`; outside a project, uses global config. See [server or Docker](/guide/remote/setup/#server-or-docker). Exits with status 1 when any check fails. Missing logins are warnings, not failures.

## remote login

Log an agent in on a remote, where its remote agents find the login.

```bash
workrex remote login <name> <agent> [-- <args>...]
workrex remote login <name> <agent> --import
workrex remote login <name> claude --setup-token
```

**Arguments:**

- `<name>` - Remote name from `remotes:` in the global config
- `<agent>` - `claude`, `codex`, `gemini`, `opencode`, `pi` or `omp`
- `<args>...` - Extra arguments for the agent's login command, after `--`

**Options:**

- `--import` - Copy this machine's credential files for the agent instead of logging in. Cannot be combined with `--setup-token` or extra arguments.
- `--setup-token` - Claude only: run `claude setup-token` and keep the token it prints for every Claude agent using the remote's node root

Without `--import`, runs the agent's own login flow on the remote, attached to your terminal: in the agent's sandbox image with `container` isolation, or directly on the host when sandboxing is off or `isolation: none` is set. Browser logins that call back to `localhost` get their ports forwarded from the local machine. See [logins and secrets](/guide/remote/logins/) for each agent's flow and the limits of `--import`.

## remote pull

Bring a remote workspace home.

```bash
workrex remote pull <handle> [--no-hooks]
```

**Arguments:**

- `<handle>` - Handle of the remote workspace

**Options:**

- `--no-hooks` - Skip `post_create` hooks for the local worktree

Stops the remote agent, recreates the workspace as a local worktree with the same branch, staged changes, unstaged changes and untracked files, opens its window with your configured panes, and removes the remote workspace. The remote agent's conversation is not carried over; the local window starts a fresh agent. Run it from the project's repository.

If the remote can't be reached after the work came home, the remote cleanup is left pending and a later command finishes it (see [merging](/guide/remote/git/#uncommitted-work-conflicts-and-cleanup)). See [what doesn't come home](/guide/remote/git/#what-doesnt-come-home).

## remote gc

List what a remote holds that no workspace uses, and remove it.

```bash
workrex remote gc <name> [--dry-run | --force]
```

**Arguments:**

- `<name>` - Remote name from `remotes:` in the global config

**Options:**

- `--dry-run` - Only list what would be removed
- `--force` - Remove without asking, including workspaces whose work exists only on the remote. Cannot be combined with `--dry-run`.

Lists workspaces this machine has no record of, stray worktrees, Rex sessions, containers, supervisors, refs and state files, unused node binaries, and local records of workspaces the remote no longer has. Then asks before removing them. Workspaces with uncommitted changes or commits beyond their base are only removed with `--force`. Without a terminal, nothing is removed unless `--force` is given. See [cleaning up a server](/guide/remote/troubleshooting/#cleaning-up-a-server).

## remote logs

Show the output of merges and removals requested from remote panes.

```bash
workrex remote logs <name> [<request>]
```

**Arguments:**

- `<name>` - Remote name from `remotes:` in the global config
- `<request>` - Request ID, as printed in the remote pane

Without a request ID, lists the saved log files for the remote. With one, prints that request's output, including hook output. Logs are kept on the local machine, need no connection to the remote, and aren't deleted automatically. See [operation logs](/guide/remote/git/#operation-logs).

## Examples

```bash
# Install and check a new remote
workrex remote setup devbox
workrex remote doctor devbox

# Log Codex in on the remote, using device auth instead of a browser callback
workrex remote login devbox codex -- --device-auth

# Give the remote's Claude agents a long-lived token
workrex remote login devbox claude --setup-token

# Copy Gemini's credential files from this machine
workrex remote login devbox gemini --import

# Move a remote workspace to the local machine
workrex remote pull my-feature

# Preview, then remove leftovers
workrex remote gc devbox --dry-run
workrex remote gc devbox
```
