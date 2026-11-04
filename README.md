# workrex

workrex runs coding agents in parallel, each in its own git worktree, inside
[Rex](https://www.superlogical.com/rex), the terminal from Superlogical. Each
agent gets its own Rex session, its status shows in a Rex sidebar and a
dashboard, and agents can run on a Linux server over SSH. It is for developers
who use Rex and want several agents working on one repository at the same time
without the agents touching each other's files.

workrex is a fork of [workmux](https://github.com/raine/workmux) by Raine Virta
and the workmux contributors. Most of what workrex does is workmux's work. The
fork adds the Rex support and renames the binary, so `workrex` installs beside
an upstream `workmux`. See [NOTICE.md](NOTICE.md) for the license and the full
list of renames.

- [What you get in Rex](#what-you-get-in-rex)
- [Install](#install)
- [Quick start](#quick-start)
- [Set up Rex](#set-up-rex)
- [Run agents on a remote server](#run-agents-on-a-remote-server)
- [Commands](#commands)
- [Configuration keeps the workmux names](#configuration-keeps-the-workmux-names)
- [How workrex relates to workmux](#how-workrex-relates-to-workmux)
- [Contributing and credits](#contributing-and-credits)

## What you get in Rex

The Rex backend is experimental. It needs a Rex server that speaks API
revision 8 or 9.

- **A Rex backend.** workrex detects Rex from `$REX_BLOCK`, which Rex sets in
  every pane. To force it, set `WORKMUX_BACKEND=rex`. If you run tmux, WezTerm,
  Zellij or kitty inside a Rex pane, workrex uses that multiplexer instead.
- **One Rex session per agent.** Under Rex, session mode is the default.
  `workrex add` creates a Rex session for each worktree, and the worktree's
  panes are Rex blocks in it.
- **Agent status in a sidebar.** `workrex sidebar` adds a block labeled
  `workrex-sidebar` to the left of each window. It lists every agent and its
  status, and you can jump to an agent from it.
- **A dashboard in a floating layer.** A Rex Lua file opens `workrex dashboard`
  in a layer over the current tab. Press `Shift+Cmd+D` to open it.
- **Status without hooks.** workrex reads the status that programs report to
  Rex through the
  [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)
  (OSC 7501). Claude Code reports it with no setup.
- **Remote agents.** An agent can run on a Linux server over SSH, for example
  a WSL distro on a Windows machine. With `terminal: rex`, the agent's terminal
  runs on the server's own Rex server, and Rex.app shows it.

The [workrex documentation](https://kong109.github.io/workrex/) has the detail:

- [Rex backend](https://kong109.github.io/workrex/guide/rex/)
- [Session mode](https://kong109.github.io/workrex/guide/session-mode/)
- [Sidebar](https://kong109.github.io/workrex/guide/sidebar/)
- [Dashboard](https://kong109.github.io/workrex/guide/dashboard/)
- [Status tracking](https://kong109.github.io/workrex/guide/status-tracking/)
- [Remote agents](https://kong109.github.io/workrex/guide/remote/)

## Install

workrex ships prebuilt binaries for macOS on Apple silicon (`darwin-arm64`)
and Linux on x86-64 (`linux-amd64`) on the
[kong109/workrex releases page](https://github.com/kong109/workrex/releases).
Each tarball has a `.sha256` file next to it.

To install the latest release, run the install script:

```bash
curl -fsSL https://raw.githubusercontent.com/kong109/workrex/main/scripts/install-workrex.sh | bash
```

The script checks the checksum and installs `workrex` to `/usr/local/bin`, or
to `~/.local/bin` when `/usr/local/bin` is not writable. To pin a version, set
`WORKMUX_VERSION`, for example `WORKMUX_VERSION=v0.1.278-rex.1`. To pick the
directory, set `WORKMUX_INSTALL_DIR`.

To build from source, install a Rust toolchain and run:

```bash
git clone https://github.com/kong109/workrex.git
cd workrex
cargo install --path .
```

To update an installed binary from the latest kong109/workrex release, run
`workrex update`. To remove it, run
[`scripts/uninstall-workrex.sh`](scripts/uninstall-workrex.sh).

To check the install, print the version:

```bash
workrex --version
```

## Quick start

These steps start one agent in its own worktree. Run them in a Rex pane, from
inside a git repository.

1. Install the agent status hooks and the bundled skills:

   ```bash
   workrex setup
   ```

2. Optional: write an example project config to `.workmux.yaml`:

   ```bash
   workrex init
   ```

3. Create a worktree and start an agent in it with a prompt:

   ```bash
   workrex add fix-login -p "Fix the redirect loop on the login page"
   ```

   workrex creates the branch `fix-login`, a worktree for it, and a Rex
   session that runs the agent.

4. Watch the agents:

   ```bash
   workrex dashboard
   workrex status
   ```

5. Send the agent a follow-up, or wait for it to finish:

   ```bash
   workrex send fix-login "Also add a test for the redirect"
   workrex wait fix-login
   ```

6. Merge the branch into your main branch and remove the worktree and its
   session:

   ```bash
   workrex merge fix-login
   ```

   To drop the work without merging, run `workrex remove fix-login`.

To start several agents at once, use `-n`, for example
`workrex add review -n 3 -p "Review src/auth for bugs"`. The
[quick start guide](https://kong109.github.io/workrex/guide/quick-start/) and the
[workflows guide](https://kong109.github.io/workrex/guide/workflows/) cover more.

## Set up Rex

workrex needs the `rex` CLI on `PATH`. Inside a Rex pane it already is.

### Turn on Remote Control

To jump to an agent in another tab or session, turn on **Remote Control** in
Rex Settings. Without it, Rex.app refuses the jump, and the dashboard and the
sidebar show the fix. `workrex add` and closing a session work without Remote
Control.

### Open the dashboard in a floating layer

1. Copy [`resources/rex/workrex.lua`](resources/rex/workrex.lua) to
   `~/.config/rex/workrex.lua`.
2. Add this line to `~/.config/rex/init.lua`:

   ```lua
   require("workrex")
   ```

3. Check the file and reload the Rex config:

   ```bash
   rex config check ~/.config/rex/workrex.lua
   rex config reload
   ```

4. Press `Shift+Cmd+D`, or run **workrex: Open Dashboard** from the command
   palette.

The key replaces Rex's default binding for **Split Pane Down**. To use another
key, change the `rex.bind` line at the end of `workrex.lua`.

### Show the sidebar

To show the sidebar in every window, run:

```bash
workrex sidebar on
```

To limit the sidebar to the current session, run `workrex sidebar --session`.

### Use a second Rex server

workrex talks to the Rex server named by `REX_SERVER`. When `REX_SERVER` is
unset, it uses the server that the `rex` CLI finds by default. If you start a
second Rex server, set `REX_SERVER` in that server's environment, so its panes
reach it. See
[Choosing the server](https://kong109.github.io/workrex/guide/rex/#choosing-the-server).

## Run agents on a remote server

A remote agent runs on a Linux server that you reach over SSH. It still shows
in the dashboard, the sidebar, `workrex status` and `workrex list`.

1. Name the server in `~/.config/workmux/config.yaml`. `host` is an
   `~/.ssh/config` alias or `user@host`:

   ```yaml
   remotes:
     winbox:
       host: winbox-wsl-ssh
   ```

2. Install the workrex node binary and the agent status hooks on the server,
   then check the server:

   ```bash
   workrex remote setup winbox
   workrex remote doctor winbox
   ```

3. Log an agent in on the server. The agent is one of `claude`, `codex`,
   `gemini`, `opencode`, `pi` or `omp`:

   ```bash
   workrex remote login winbox claude
   ```

4. Start an agent on the server:

   ```bash
   workrex add fix-login --remote winbox -p "Fix the redirect loop"
   ```

By default the agent runs in tmux on the server, and a relay window on your
machine shows it. If the server runs its own Rex server that Rex.app reaches
over Tailscale, the agent can run there instead:

```yaml
remotes:
  winbox:
    host: winbox-wsl-ssh
    isolation: none
    terminal: rex
```

With `terminal: rex`, `workrex add --remote winbox` creates the agent's Rex
sessions on the server and opens no relay window. `terminal: rex` needs
`isolation: none`, and `workrex open` needs Remote Control.

For setup on a WSL distro, logins, git access and fixes, see these pages:

- [Remote setup, including WSL](https://kong109.github.io/workrex/guide/remote/setup/)
- [Remote usage](https://kong109.github.io/workrex/guide/remote/usage/)
- [Remote logins](https://kong109.github.io/workrex/guide/remote/logins/)
- [Remote git](https://kong109.github.io/workrex/guide/remote/git/)
- [Remote troubleshooting](https://kong109.github.io/workrex/guide/remote/troubleshooting/)

## Commands

The table lists the main commands. Run `workrex <command> --help` for the
options of each one, and `workrex --help` for the full list.

| Command                      | What it does                                                   |
| ---------------------------- | -------------------------------------------------------------- |
| `workrex add <branch>`       | Create a worktree and start its agent in a window or session   |
| `workrex open <name>`        | Open a window for an existing worktree                         |
| `workrex close <name>`       | Close a worktree's window and keep the worktree and branch     |
| `workrex merge [name]`       | Merge a branch, then remove its worktree and window            |
| `workrex remove <name>`      | Remove a worktree, its window and its branch without merging   |
| `workrex rebase [name]`      | Rebase a worktree branch onto its base branch                  |
| `workrex rename`             | Rename a worktree, its window or session, and maybe its branch |
| `workrex resurrect`          | Reopen worktree windows after a crash or a server restart      |
| `workrex list`               | List all worktrees                                             |
| `workrex status`             | Show agent status for worktrees                                |
| `workrex dashboard`          | Open a TUI dashboard of all agents                             |
| `workrex sidebar`            | Turn the agent status sidebar on or off                        |
| `workrex send <name> [text]` | Send a prompt to a running agent                               |
| `workrex capture`            | Print terminal output from a running agent                     |
| `workrex wait <names>...`    | Wait for agents to reach a status, `done` by default           |
| `workrex run`                | Run a command in a worktree's window                           |
| `workrex remote`             | Set up, check and log in to SSH remotes                        |
| `workrex setup`              | Install status tracking hooks and skills                       |
| `workrex init`               | Write an example `.workmux.yaml`                               |
| `workrex config`             | Edit or print the global config, or print its reference        |
| `workrex sandbox`            | Manage sandbox settings                                        |
| `workrex update`             | Update to the latest kong109/workrex release                   |
| `workrex docs`               | Show this README in the terminal                               |

## Configuration keeps the workmux names

Only the binary is renamed. workrex reads the same configuration as workmux:

- Global config: `~/.config/workmux/config.yaml`. Run `workrex config path` to
  print the path.
- Project config: `.workmux.yaml` in the repository root.
- Environment variables: `WORKMUX_*`, for example `WORKMUX_BACKEND`.
- Cache and state directories named `workmux`.

The names stay the same for two reasons. An existing workmux setup works with
workrex without changes, and the fork stays easy to rebase on upstream.

A few files that both binaries would write get workrex names, so the two
installs don't overwrite each other. Examples are the remote node root
`~/.workrex-node` and the agent hook files named `workrex-status`.
[NOTICE.md](NOTICE.md) lists every rename.

To print every option with its default, run `workrex config reference`. The
[configuration guide](https://kong109.github.io/workrex/guide/configuration/) describes the options.

## How workrex relates to workmux

workrex tracks upstream workmux and rebases on it. Everything outside the Rex
support comes from upstream and works under the `workrex` name. That includes
the tmux, WezTerm, kitty and Zellij backends, the worktree commands, merge,
agent support and the sandbox.

The [workrex documentation](https://kong109.github.io/workrex/) covers those features too:

- [Agents](https://kong109.github.io/workrex/guide/agents/)
- [Workflows](https://kong109.github.io/workrex/guide/workflows/)
- [Sandbox](https://kong109.github.io/workrex/guide/sandbox/)
- [Git worktree caveats](https://kong109.github.io/workrex/guide/git-worktree-caveats/)

The kong109/workrex repository has no issue tracker. If a problem also
happens with upstream `workmux`, report it to
[raine/workmux](https://github.com/raine/workmux/issues).

## Contributing and credits

See [CONTRIBUTING.md](CONTRIBUTING.md) for the build and the checks. It is
upstream's guide, so its clone URL and binary name are workmux's. To keep the
fork's README from conflicting on each upstream rebase, see
[Rebasing on upstream](NOTICE.md#rebasing-on-upstream).

workrex is built on [workmux](https://github.com/raine/workmux) by Raine Virta
and the workmux contributors, under the MIT License. The upstream copyright and
license text are in [LICENSE](LICENSE). [NOTICE.md](NOTICE.md) has the credits
and the list of renames.
