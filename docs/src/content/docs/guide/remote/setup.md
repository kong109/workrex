---
title: "Setting up a server"
description: Prepare a Linux server, add it to your config and choose how its agents run
---

## Requirements

- Linux on x86-64. workrex publishes release builds for linux-amd64 only. For an ARM64 server, build a Linux ARM64 `workrex` yourself and point [`remotes.<name>.binary`](/guide/configuration/#remotes) at it.
- SSH that works without a prompt. workrex never asks for a password or to accept a host key, so use a key loaded in your SSH agent or one without a passphrase, and run `ssh <host>` once yourself. Aliases from `~/.ssh/config`, including `ProxyJump`, `Port` and `User`, work.
- Git and a running Rex server reachable by Rex.app.
- systemd linger for the SSH user, so agents survive the end of the SSH session:

  ```bash
  sudo loginctl enable-linger <user>
  ```

- Network access to the APIs your agents use.

The server doesn't need access to GitHub. The local machine downloads `workrex-linux-amd64.tar.gz` from the [kong109/workrex releases](https://github.com/kong109/workrex/releases), checks it against its `.sha256` file and uploads it. What else the server needs depends on [where agents run](#server-or-docker).

## Add the server to your config

```yaml
# ~/.config/workrex/config.yaml (global config only)
remotes:
  devbox: # the name you use on the command line
    host: devbox # an ~/.ssh/config alias or user@host
```

These are the Workrex configuration paths. See [configuration](/guide/configuration/).

Names may contain letters, digits, `.`, `_` and `-`, and must start with a letter or digit. `local` is reserved.

`remotes` only works in your global config. A project's `.workrex.yaml` can't add a server, so a repository can't send your code anywhere; it can only choose among your servers with [`placement`](#choosing-where-new-agents-go). The other keys, `path`, `isolation`, `binary`, `terminal` and `rex_client`, are in the [configuration reference](/guide/configuration/#remotes).

## A server in WSL

A Linux distro in Windows Subsystem for Linux works as a server. Run sshd inside the distro on its own port, because port 22 on the Windows machine usually belongs to Windows OpenSSH, which can't run agents. This example uses port 2222 on a Windows machine named `winpc` (its Tailscale or LAN name) and the WSL user `dev`.

1. In WSL, turn on systemd, so linger and the `ssh` socket work. Add this to `/etc/wsl.conf`, then run `wsl.exe --shutdown` on Windows and open the distro again:

   ```ini
   [boot]
   systemd=true
   ```

2. In WSL, install the OpenSSH server, move the `ssh.socket` unit from port 22 to 2222, and turn on linger:

   ```bash
   sudo apt install openssh-server
   sudo systemctl edit ssh.socket
   sudo systemctl restart ssh.socket
   sudo loginctl enable-linger dev
   ```

   In the editor that `systemctl edit` opens, add this drop-in. The empty `ListenStream=` line clears the default port 22. Without it, the socket listens on port 22 and port 2222. Give both addresses: Ubuntu's `ssh.socket` sets `BindIPv6Only=ipv6-only`, so a bare `ListenStream=2222` accepts IPv6 connections only.

   ```ini
   [Socket]
   ListenStream=
   ListenStream=0.0.0.0:2222
   ListenStream=[::]:2222
   ```

   With WSL's mirrored networking, port 2222 in WSL is port 2222 on Windows. Otherwise forward it from Windows to WSL, or reach the distro through [Tailscale SSH](https://tailscale.com/kb/1193/tailscale-ssh) running inside it.

3. On your machine, add an alias to `~/.ssh/config` and check that `ssh wsl-ssh` logs in without a prompt:

   ```
   Host wsl-ssh
     HostName winpc
     Port 2222
     User dev
   ```

4. Point the remote at the alias:

   ```yaml
   # ~/.config/workrex/config.yaml
   remotes:
     wsl:
       host: wsl-ssh
       isolation: none
   ```

   `isolation: none` runs agents directly in the distro, so the server needs no Docker. See [server or Docker](#server-or-docker).

5. Install workrex on the server and check it:

   ```bash
   workrex remote setup wsl
   workrex remote doctor wsl
   ```

   `setup` downloads the linux-amd64 release on your machine, uploads it and prints where it installed it:

   ```
   Downloading workrex v<version> for linux-amd64...
   Uploading workrex-<version> (<size> MB) to wsl...
   ✓ wsl: installed workrex <version> (protocol <n>) at ~/.workrex-node/bin/workrex-<version>
   ```

   When every `doctor` check passes, [log your agents in](/guide/remote/logins/).

`workrex remote doctor wsl` names the WSL fix when the connection fails: Windows OpenSSH answering on the configured port, or nothing answering because the distro or its `ssh.socket` is down. It also names the systemd fix when systemd is not running in the distro, and `loginctl enable-linger` when systemd runs but linger is off.

## Agent terminals on the server's Rex

All remote terminals run in Rex. Install Rex on the Linux server if needed:

```bash
curl -fsSL https://pkgs.superlogical.com/rex/install.sh | sh -s -- --tarball
```

The binary-only installer does not configure a running service. Follow the
[Rex Linux installation guide](https://www.superlogical.com/rex/docs/linux/installation)
to run the server and connect Rex.app. The server must be reachable when the
workspace starts. Workrex checks its server protocol before creating terminals.

```yaml
remotes:
  wsl:
    host: wsl-ssh
    isolation: none
    terminal: rex
```

`rex` is the only supported terminal value and the default. Workrex creates one session for the workspace on that server, applies its
configured pane layout, and records the final block IDs, so renamed sessions
remain associated with the execution. `workrex open <name>` shows the target in
Rex.app and needs Remote Control. When several app clients are connected, set
`remotes.<name>.rex_client` to the requested client's label.

- Secrets reach agent blocks through read-once files, rather than session
  labels or command lines.
- `workrex shell` creates another native Rex session for an ad-hoc workspace
  shell, with the same isolation unless `--host` is passed.
- If the server restarts and its sessions are gone, `workrex open` starts a new
  execution using the agent's resume arguments.

## Server or Docker

Remote agents are sandboxed exactly when the same agent would be sandboxed locally. `remotes.<name>.isolation` overrides that for one server, in either direction.

| `sandbox.enabled` | `remotes.<name>.isolation` | Agents run    |
| ----------------- | -------------------------- | ------------- |
| off (default)     | unset                      | on the server |
| on                | unset                      | in Docker     |
| any               | `none`                     | on the server |
| any               | `container`                | in Docker     |

The two modes need different things on the server and behave differently in a few places:

|                                           | On the server                                     | In Docker                                          |
| ----------------------------------------- | ------------------------------------------------- | -------------------------------------------------- |
| Agent CLIs                                | Install each one on the SSH user's login `PATH`   | Come from the sandbox image                        |
| Also needs                                | Your project's tools, if hooks or agents use them | Docker, usable without `sudo`; access to `ghcr.io` |
| Sandbox                                   | None, with the agent's normal permission prompts  | The [container sandbox](/guide/sandbox/container/) |
| `workrex rm` and `merge` in a remote pane | Ask the local machine to do it                    | Agents can ask the local machine, without hooks    |
| `sandbox.env` and `env_passthrough`       | Not used                                          | Used                                               |

workrex doesn't install agents on the server and doesn't fall back to Docker: if the agent's CLI is missing on the server, `add --remote` stops before creating anything. Podman isn't supported on servers.

:::danger[No sandbox on the server]
Agents running directly on the server have the SSH user's full access to it. Only use this mode on a machine you'd let the agent use freely.
:::

Run `remote setup`, `remote doctor` and `remote login` from the same project directory as `add`, because the project's `sandbox.enabled` decides the mode. Outside a project they use your global settings. `add` and `doctor` print the mode and the setting that chose it.

Each workspace keeps the mode it was created with, even if your settings change later.

## Install and check

```bash
workrex remote setup devbox
```

Installs the workrex that matches your local version on the server, under the node root (`~/.workrex-node` by default, set with [`path`](/guide/configuration/#remotes)). Each version lives at `bin/workrex-<version>` in the node root, so running agents keep the version they started with. `setup` also installs workrex's [status hooks](/guide/status-tracking/) for the supported agents and, in server mode, writes `~/.local/bin/workrex`, which runs the pinned version from your own shells on the server (see [shells on the server](/guide/remote/usage/#commands-on-remote-workspaces)). It is safe to run again. `add --remote` and `remote login` install workrex on first use too, but running `setup` first shows problems early.

```bash
workrex remote doctor devbox
```

Checks SSH, the server's architecture, workrex, Git, Rex, linger, agent CLIs or Docker, status hooks, the `workrex` on the login `PATH` and logins, and prints a fix for each failing check. In Docker mode it also pulls the sandbox image, so the first run can take a while. See [`remote doctor`](/reference/commands/remote/#remote-doctor) for every check.

```bash
workrex remote
```

Lists your servers and whether each one is reachable and set up.

Then [log your agents in](/guide/remote/logins/).

## Choosing where new agents go

Agents are created locally unless you ask otherwise:

- `workrex add <branch> --remote [<name>]` creates the agent on a server. The name is optional when `placement` names a server or only one is configured.
- `workrex add <branch> --local` creates it locally, overriding `placement`.
- `placement` sets the default: `local` (the default) or a server name.

```yaml
# ~/.config/workrex/config.yaml or .workrex.yaml
placement: devbox
```

A project's `placement` wins over the global one, so a project can send its agents to a server or keep them local. If `placement` names a server that isn't configured, `add` fails and suggests `--local`.

Where a workspace lives is decided when it is created. Later commands such as `open`, `merge` and `remove` find it by its handle. The dashboard creates local agents only, and refuses when `placement` names a server.

## What the server uses from your config

A remote workspace runs a copy of the local machine's effective config, taken when the workspace is created. The server's own workrex config and the worktree's `.workrex.yaml` are never read. `add --remote` warns about settings it couldn't apply.

The local machine also saves the creator's effective config for autonomous
merge, rebase, and remove requests. These requests keep using the saved hooks and
connection after the launching command exits; later edits to the project config
do not change that running job. Terminal actions use its recorded connection and
Rex client, including when invoked from another project. Removing a remote from
the current profile does not disconnect an already recorded workspace.

- **Applied:** `panes`, `agent`, `agents`, `post_create`, `pre_remove`, `remote_files`, the status icons and format, and `hook_shell` unless it is an absolute local path. An absolute `hook_shell` such as `/opt/homebrew/bin/bash` names a local program, so remote hooks run with `bash -c` instead.
- **Applied in Docker mode:** `sandbox.image`, `target`, `network`, `toolchain`, `env`, `env_passthrough`, and `container.cpus`, `memory`, `group_add`, `cap_add`, `security_opt` and `excluded_files`. The sandbox is always Docker there.
- **Not applied**, because they name local resources: Lima, other container runtimes, `rpc_host`, `extra_mounts`, `agent_config_dir`, `host_commands`, `container.devices` and `container.oci_runtime`.

Two routine adjustments are not shown on every `add`: falling back to `bash -c` for an absolute `hook_shell`, and leaving local `files` behind when `remote_files` is not configured. Both are recorded in the workrex log, `$XDG_STATE_HOME/workrex/workrex.log` (`~/.local/state/workrex/workrex.log` by default), as lines starting with `remote config:`:

```bash
grep 'remote config:' ~/.local/state/workrex/workrex.log
```

`post_create` and `pre_remove` hooks run on the server, in the remote worktree. If `add --remote` fails partway, run it again with the same arguments: it resumes and doesn't repeat hooks that already finished.

## Supplemental files

The Git checkout is transferred automatically. Extra local files are sent only when selected in `remote_files`; the local `files` settings do not apply remotely.

```yaml
files:
  symlink:
    - tests/venv # Shared locally, never uploaded by this setting

remote_files:
  copy:
    - .env
    - CLAUDE.local.md
  symlink:
    - shared-data
```

- **`copy`** sends the current local contents into each laptop-created workspace.
- **`symlink`** seeds a shared directory on the remote, then links from the workspace. Existing sources are reused, not refreshed. Changes through a link are shared between remote workspaces.

Both lists accept globs and resolve paths relative to the configuration directory, just like `files`. Remote symlink patterns also match existing sources in the remote project's shared directory. The combined upload limit is 64 MiB per workspace; sources exceeding the remaining budget are skipped with a warning. Selecting a local symlink uploads its target's contents.

Without `remote_files`, no supplemental files are uploaded, including ignored `CLAUDE.local.md`. Global `remote_files` settings are inherited independently of `files`. Project lists replace the corresponding global lists unless they include `"<global>"`. To disable both inherited lists explicitly:

```yaml
remote_files:
  copy: []
  symlink: []
```

An empty object (`remote_files: {}`) still inherits global lists. When local file settings exist but remote lists are unset, workrex records a `remote config:` note in the log; explicit empty lists suppress it.

Selection changes apply to new workspaces, not existing ones, and do not delete cached sources. Workspaces spawned by a remote agent reuse their parent's saved copies and symlink selections without contacting the laptop. Resuming creation does not repeat an already completed file stage.

## Security

Treat the server as an extension of your local machine: the SSH user runs every agent and holds their logins. Use a server or VM dedicated to agents, and treat what agents produce there as untrusted, as with a [local sandbox](/guide/sandbox/#security-model).

- **No host commands.** [Host command proxying](/guide/sandbox/features/#host-command-proxying) is off on servers. In Docker mode, agents can only run what their container has.
- **No Git credentials.** The server has no `origin`, and workrex connects without SSH agent or X11 forwarding. Anything else you give agents, such as tokens in `sandbox.env`, is up to you.
- **Local actions stay local.** A remote pane or agent can ask the local machine to remove or merge its own workspace, and nothing else; hook output from that work stays on the local machine. A request from a container runs no hooks. `workrex close` and image paste fail with a message saying what to run on the local machine.
- **Hooks run outside the container.** `post_create` and `pre_remove` run on the server as the SSH user, as they run on the local machine for local agents. They come from your local config.
- **Firewall the Docker bridge** if the server routes or forwards it to the outside. The sandbox's RPC bridge and proxy listen on the bridge address.

workrex marks remote worktrees as trusted in Claude and Codex, so they start without asking whether to trust the folder. Folders you marked untrusted stay untrusted. A trusted folder lets the agent load the repository's own agent settings, such as `.codex/config.toml`.
