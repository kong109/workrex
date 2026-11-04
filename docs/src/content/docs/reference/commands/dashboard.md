---
title: "dashboard"
description: Open a TUI dashboard showing all active AI agents
---

Opens a TUI dashboard showing all active AI agents across all multiplexer sessions.

```bash
workrex dashboard
```

## Options

- `-d, --diff`: Open the diff view directly for the current worktree's agent.
- `-P, --preview-size <10-90>`: Set preview pane size as percentage (larger = more preview, less table). Default: 60.
- `-s, --session`: Filter to only show agents in the current session.
- `-t, --tab <agents|worktrees>`: Open directly on the specified tab.

## Examples

```bash
# Open dashboard with default layout
workrex dashboard

# Open with smaller preview pane (40% of height)
workrex dashboard --preview-size 40

# Open diff view directly for current worktree
workrex dashboard --diff

# Show only agents in the current tmux session
workrex dashboard --session

# Open directly on the Worktrees tab
workrex dashboard --tab worktrees
```

## Under Rex

Under [Rex](/guide/rex/), choosing an agent moves Rex.app to the agent's tab and focuses its block. The jump asks the Rex.app client that owns your pane, and it needs Remote Control, which you turn on in Rex Settings. Without it, the dashboard stays open and shows the fix in its footer. Enter on a worktree row moves Rex.app to the worktree's agent, or in session mode to the worktree's session. A window-mode worktree with no agent is opened but Rex.app does not move.

To open the dashboard in a floating layer over the current tab, the Rex counterpart of a tmux popup, load `resources/rex/workrex.lua` from your Rex `init.lua`. The layer binds `Shift+Cmd+D` and passes its `args` to `workrex dashboard`. See [Rex dashboard](/guide/rex/#dashboard) for the setup.

See the [Dashboard guide](/guide/dashboard/) for keybindings and detailed documentation.
