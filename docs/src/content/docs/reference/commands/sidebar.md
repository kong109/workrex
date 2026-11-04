---
title: "sidebar"
description: Control a live agent status sidebar in tmux or Rex
---

Controls a live agent status sidebar on the left or top edge of tmux windows, or on the left of Rex windows. By default, each sidebar pane shows active agents across all
sessions with live status updates. Use `workrex sidebar filter session` to show
only agents in the current tmux session.

```bash
workrex sidebar                         # Toggle sidebar on/off (all sessions)
workrex sidebar on                      # Ensure the global sidebar is running
workrex sidebar off                     # Ensure the global sidebar is stopped
workrex sidebar --session on            # Ensure it is running in this session
workrex sidebar --session off           # Ensure it is stopped in this session
workrex sidebar on --position top       # Enable with a top sidebar
workrex sidebar on --width 40           # Set left sidebar width in columns
workrex sidebar on --height 3           # Set top sidebar height in rows
```

## What it shows

Each agent row displays:

- Status icon (working/waiting/done with spinner animation)
- Project and worktree name (e.g. `myproject/fix-bug`)
- Elapsed time since last status change
- Git diff stats, plus optional pull request number and check status in custom templates

## Keybindings

| Key     | Action                    |
| ------- | ------------------------- |
| `j`/`k` | Navigate up/down          |
| `Enter` | Run the configured action |
| `o`     | Focus the agent pane      |
| `g`/`G` | Jump to first/last        |
| `v`     | Toggle layout mode        |
| `f`     | Toggle session filter     |
| `V`     | Toggle rule-based views   |
| `z`     | Toggle sleeping on agent  |
| `t`     | Toggle grouping           |
| `h`/`l` | Unfold/fold current group |
| `s`     | Toggle the current fold   |
| `S`     | Fold/unfold every group   |
| `?`     | Show these keys           |
| `q`     | Open quit confirmation    |

## Mouse support

With tmux mouse mode enabled (`set -g mouse on`), click an agent row or top-bar
chip to jump to its pane, or scroll to navigate the list.

## Navigation commands

Switch between agents from any tmux pane, in the same order shown in the
sidebar. Navigation uses the same filter mode as the sidebar view, so it cycles
through all sessions by default:

| Command                         | Action                                               |
| ------------------------------- | ---------------------------------------------------- |
| `workrex sidebar next`          | Switch to the next agent (wraps)                     |
| `workrex sidebar prev`          | Switch to the previous agent (wraps)                 |
| `workrex sidebar jump <N>`      | Jump to the Nth agent (1-indexed)                    |
| `workrex sidebar filter`        | Toggle session filter (none/session)                 |
| `workrex sidebar filter <MODE>` | Set filter mode: `none`/`all` or `session`/`project` |
| `workrex sidebar group`         | Toggle grouping between grouped and flat             |
| `workrex sidebar group <MODE>`  | Set grouping: `none`/`off`, `project` or `session`   |
| `workrex sidebar group --clear` | Drop the runtime grouping and follow config again    |

### Example tmux keybindings

```bash
# Alt+j / Alt+k to cycle agents (no prefix needed)
bind -n M-j run-shell "workrex sidebar next"
bind -n M-k run-shell "workrex sidebar prev"

# Alt+1..9 to jump directly
bind -n M-1 run-shell "workrex sidebar jump 1"
bind -n M-2 run-shell "workrex sidebar jump 2"
bind -n M-3 run-shell "workrex sidebar jump 3"
# ...

# Or with prefix key (avoids terminal conflicts)
bind C-j run-shell "workrex sidebar next"
bind C-k run-shell "workrex sidebar prev"
```

## Configuration

```yaml
stale_after: 5h # dashboard and sidebar timeout; default: 1h

sidebar:
  position: left # "left" (default) or "top"
  width: 40 # left width in columns (default: "10%", clamped 25-50)
  # width: "15%"
  layout: tiles # left only: "compact" or "tiles" (default)
  enter_action: focus # "focus" (default) or "select"
  git_status: true # collect Git status and GitHub PR/check data (default)
  dim_stale: true # dim stale agents using stale_after
  group_by: project # "project" or "session"; unset keeps one flat list
  collapse_stale: true # while grouped, fold stale agents behind a toggle
```

Set `enter_action: select` to show the selected agent's window while keeping
focus in its sidebar, so `j`/`k` can continue scanning agents. Press `o` to
focus the selected agent's pane. If the target window has no sidebar, Enter
falls back to focusing the agent pane. Mouse clicks always focus the pane.

For a horizontal top bar:

```yaml
sidebar:
  position: top
  height: 3 # top height in rows
  horizontal:
    item_width: 24 # horizontal chip width in columns (default, clamped 12-80)
  templates:
    horizontal:
      - "{status_icon} {primary} {pane_suffix} {fill} {elapsed}"
      - "{secondary} {fill} {git_stats}"
      - "{pane_title}"
```

Set `sidebar.git_status: false` in the global config to stop the sidebar daemon
from watching worktrees and collecting Git status. Git status, pull request, and
check tokens then render empty. This setting is daemon-wide and does not affect
the dashboard's independent Git status updates.

Explicit width values bypass the default 25-50 column clamp (minimum 1
column). Layout preference can also be toggled at runtime with `v` and is
persisted across restarts. Height only applies to `position: top`; set it as a
row count for the number of horizontal lines you want to show. The top bar uses a
horizontal chip layout, so `v` has no effect there. Horizontal templates render
as many configured lines as the current height allows. `horizontal.item_width`
controls each chip width and is clamped between 12 and 80 columns. Position
changes take effect when the sidebar is enabled. Use
`workrex sidebar on --position top` or `--position left` to override the
configured placement. Width and height also accept percentages, such as
`workrex sidebar on --width 15%`. Repeating `on` with appearance options applies
them without creating duplicate panes. The `off` action rejects position and
dimension flags because they have no effect while stopping the sidebar.

## Named views

Use named views to show agents from selected sessions or project directories.
Add `sidebar.views` to your global config at `~/.config/workmux/config.yaml`
(or `$XDG_CONFIG_HOME/workmux/config.yaml`). Views cannot be set per project.
Config and state paths keep their workmux names so existing workmux configs work. See [Configuration](/guide/configuration/).

```yaml
sidebar:
  views:
    - name: Named sessions
      sessions: [foo, bar, xyz]
    - name: Code
      project_dirs: [~/code]
    - name: Work
      project_dirs: [~/work]
    - name: Other
      other: true
```

Views are enabled by default when configured. Each window uses the first view
matching its agent's session or project, then Other if nothing matches,
or All if there is no Other view. Switching to another window shows that
window's view without changing the one you left.

In a window with several agents, the view follows the active agent and stays
on that view when you focus the sidebar. A window without a registered agent
uses its oldest pane's session and working directory.

### Turning views on and off

Press `V` to toggle views for every sidebar in the tmux server:

- **On:** each window shows agents matching its own view.
- **Off:** every sidebar shows all agents.

```bash
workrex sidebar view       # Toggle views on/off
workrex sidebar view on    # Apply each window's matching rules
workrex sidebar view off   # Show all agents in every sidebar
```

The view name appears at the bottom of the sidebar, or All when views are off.
A view with no matching agents shows "No matching agents". Turning the sidebar
off and on preserves the toggle; restarting tmux enables views again.

The `f` session filter works in both modes. When it is on, each sidebar shows
only agents in its current session, whether or not views are enabled.
Views do not change row grouping (`sidebar.group_by`) or which windows have a
sidebar (`sidebar --session`). `next`, `prev`, and `jump` follow the current
window's view once the sidebar updates.

A top sidebar needs at least two rows: one for the view name and one for agents.
With no views configured, `V` does not change which agents appear.

### Matching rules

- `sessions` matches exact, case-sensitive tmux session names. `foo` does not
  match `foobar` or `Foo`; wildcards are not supported.
- `project_dirs` matches each directory and its subdirectories. `~/code` does
  not match `~/code-old`. Use absolute paths or `~` for your home directory;
  environment variables such as `$HOME` are not expanded. Symlinks are resolved
  for paths that exist.
- Linked worktrees belong to the main repository's directory, even if the
  worktree is stored elsewhere. Bare repositories use the bare directory. If
  workrex cannot identify a repository, it uses the agent's recorded working
  directory. After moving a repository, allow about 30 seconds for views to update.
- An agent matches a view if it matches any of its session names or project
  directories. The same agent can appear in more than one view.
- `other: true` shows agents that match none of the other views. Only one view
  can use it, and that view cannot also have `sessions` or `project_dirs`.
  Without an Other view, unmatched agents appear only in All.
- Give each view a unique name other than `All`, without leading or trailing
  spaces. Each view needs `sessions`, `project_dirs`, or `other: true`.

## How it works

When enabled, a background daemon polls tmux state every 2 seconds and pushes
snapshots to each sidebar pane over a Unix socket. The sidebar creates a tmux
pane on the configured edge of every existing window. A tmux hook
(`after-new-window`) ensures newly created windows also get a sidebar
automatically.

Running `workrex sidebar` again disables the sidebar globally, killing all
sidebar panes, the daemon, and removing hooks. `workrex sidebar on` and
`workrex sidebar off` provide idempotent alternatives for scripts and tmux
configuration.

### Session-scoped mode

By default, the sidebar appears in all tmux sessions. Use `--session` to scope
it to the current session only, leaving other sessions untouched:

```bash
workrex sidebar --session on   # Ensure it is enabled in the current session
workrex sidebar --session off  # Ensure it is disabled in the current session
```

You can enable session-scoped sidebars in multiple sessions independently. Each
session can be toggled or explicitly enabled and disabled without affecting
others.

If the global sidebar is already active, `workrex sidebar --session` hides the
sidebar in the current tmux session only. Run it again to show the sidebar in
that session again while other sessions remain globally managed.

Starting global mode still replaces any session-scoped sidebars.

## Under Rex

Under [Rex](/guide/rex/#sidebar), each window in scope gets a full-height block labeled `workrex-sidebar` on its left. `--session`, `on`, `off`, `next`, `prev`, `jump`, `filter`, `group` and `view` behave as on tmux, with these differences:

- The daemon follows `rex events` instead of tmux hooks, and adds a sidebar to each new window. A sidebar you close in one window stays closed there until the next `workrex sidebar on`.
- The sidebar opens on the left. `--position top` and `--height` need tmux.
- Without a configured `sidebar.width`, dragging a sidebar border saves the new width for every sidebar.
- A jump moves Rex.app and needs Remote Control in Rex Settings. When the app refuses, `next`, `prev` and `jump` print `Rex.app did not switch: Remote Control is off. Turn it on in Rex Settings.` and exit with status 1.
- Sidebar preferences are kept in files under `~/.local/state/workmux/sidebar/`, one set per Rex server.

## Limitations

- The sidebar supports tmux and Rex. Other backends are not supported.

## Example tmux binding

```bash
bind C-t run-shell "workrex sidebar"
```
