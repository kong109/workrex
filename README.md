# workrex

Workrex runs coding agents in parallel inside [Rex](https://www.superlogical.com/rex).
Each task gets a Git worktree, branch, and Rex session. The dashboard and sidebar
show agent status, terminal output, and changes across projects. Agents can also
run on a configured Linux server over SSH and appear in Rex.app.

## Install

Install Rex from [Superlogical](https://www.superlogical.com/rex), then install
Workrex from its [GitHub releases](https://github.com/kong109/workrex/releases):

```bash
curl -fsSL https://raw.githubusercontent.com/kong109/workrex/main/scripts/install-workrex.sh | bash
workrex --version
```

Prebuilt Workrex binaries support Apple Silicon macOS and x86-64 Linux. The
installer checks the release's SHA-256 checksum. `WORKREX_VERSION` selects a
release and `WORKREX_INSTALL_DIR` selects the installation directory.

To build from source:

```bash
cargo install --git https://github.com/kong109/workrex --bin workrex
```

Update a release installation with `workrex update`. Source installations can
rerun the Cargo command.

## Quick start

Run these commands from a Rex terminal inside your Git repository:

```bash
# Install agent status hooks and bundled skills.
workrex setup

# Write .workrex.yaml, then select your agent in it.
workrex init

# Start an agent in its own branch, worktree, and Rex session.
workrex add fix-login -p "Fix the login redirect and verify it"

# Inspect agents and press Enter to show one in Rex.app.
workrex dashboard

# Merge the branch and remove the task's worktree and Rex session.
workrex merge fix-login
```

Workrex defaults to one Rex session per worktree. A Rex CLI window is a tab in
a session; each terminal pane is a block. Set `mode: window` when tasks should
open as tabs inside an existing session. `--parent-session <label>` applies
only to window mode and identifies an explicitly chosen Rex session.

`add` and `open` create or select their managed target on the server without
moving Rex.app away from the tab you are viewing. Dashboard and sidebar jumps
show the target in the app. Jumps across tabs require Remote Control in Rex
Settings.

## Configure the workspace

Global defaults live in `~/.config/workrex/config.yaml`. Project settings in
`.workrex.yaml` override them. Configuration, cache, state, and `WORKREX_*`
environment variables belong to Workrex.

```yaml
agent: codex
mode: session
window_prefix: wr-
panes:
  - command: <agent>
    focus: true
  - split: horizontal
files:
  copy:
    - .env
  symlink:
    - node_modules
post_create:
  - npm install
```

Use `workrex config path` to locate global configuration and
`workrex config reference` to print the installed version's full reference.

## Dashboard and sidebar

`workrex dashboard` lists managed worktrees and agents. Press Enter to jump to
a target, inspect its diff, or send a prompt without leaving the dashboard.

For a floating dashboard over the current Rex tab, copy
[`resources/rex/workrex.lua`](resources/rex/workrex.lua) to
`~/.config/rex/workrex.lua`, then load it from `~/.config/rex/init.lua`:

```lua
require("workrex")
```

Check and reload the configuration:

```bash
rex config check ~/.config/rex/workrex.lua
rex config reload
```

The default shortcut is `Shift+Cmd+D`. `workrex sidebar on` adds a full-height
sidebar block to the left of windows in scope. The sidebar observes Rex events
and stores per-server preferences under `~/.local/state/workrex/sidebar/`.

Agents that report Rex program status, including Claude Code, appear without
status hooks. `workrex setup` installs hooks for supported agents and these
skills: `workrex`, `merge`, `rebase`, `worktree`, `coordinator`, and `open-pr`.
Codex discovers them in `~/.agents/skills`; invoke them with `$workrex` and so on.

## Run agents on a remote server

Prepare a Linux server with Git, Rex, and noninteractive SSH access. Its Rex
server must be reachable by Rex.app. Add it to your global configuration:

```yaml
remotes:
  devbox:
    host: dev@devbox.example.com
    isolation: none
    terminal: rex
```

Then install Workrex on the node, check it, log the agent in, and start a task:

```bash
workrex remote setup devbox
workrex remote doctor devbox
workrex remote login devbox codex
workrex add fix-parser --remote devbox -p "Fix the parser and run its tests"
```

The remote workspace owns its Rex terminals. Agents keep running when the local
machine disconnects. Use `workrex open fix-parser` to show the running workspace,
and `workrex merge fix-parser` to bring its work home and merge locally.

## Main commands

| Command | Purpose |
| --- | --- |
| `add <branch>` | Create a worktree and start its agent in Rex |
| `open <handle>` | Find or recreate the worktree's managed Rex target |
| `close <handle>` | Close the managed terminal while keeping local files |
| `merge <handle>` | Merge and clean up the task |
| `remove <handle>` | Remove the task without merging |
| `list`, `status` | Inspect worktrees and agents |
| `send`, `capture`, `wait`, `run` | Communicate with agents and run checks |
| `dashboard`, `sidebar` | Inspect and navigate agents in Rex |
| `remote` | Set up and maintain SSH-connected workspaces |
| `sandbox` | Configure container or VM isolation |
| `setup` | Install agent hooks and skills |
| `config`, `init` | Inspect or edit configuration |
| `resurrect` | Reopen managed targets after a Rex server restart |

Run `workrex <command> --help` for current options. See the
[documentation](https://kong109.github.io/workrex/) for
[configuration](https://kong109.github.io/workrex/guide/configuration/),
[Rex behavior](https://kong109.github.io/workrex/guide/rex/),
[agent workflows](https://kong109.github.io/workrex/guide/workflows/),
[remote workspaces](https://kong109.github.io/workrex/guide/remote/), and
[sandboxing](https://kong109.github.io/workrex/guide/sandbox/).

## Contributing and credits

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and verification, and
[NOTICE.md](NOTICE.md) for attribution. Workrex builds on the MIT-licensed
terminal-worktree project originally authored by [Raine Virta](https://github.com/raine)
and its contributors. The original copyright and license remain in
[LICENSE](LICENSE).
