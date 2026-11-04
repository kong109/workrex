---
title: "Session mode"
description: Create worktrees in their own tmux or Rex sessions instead of windows
---

In window mode, workrex creates a **window** for each worktree in your current session. In session mode, each worktree gets its own **session** instead, with its own window list, history and layout. Use session mode when you want several windows per worktree, or when you want worktrees kept apart.

The default depends on the backend:

- Under tmux, the default is window mode.
- Under [Rex](/guide/rex/#session-mode), the default is session mode, and each worktree gets its own Rex session.

## Enabling session mode

Session mode needs tmux or Rex. To turn it on for one project, set it in the project config:

```yaml
# .workmux.yaml
mode: session
```

To turn it on everywhere, set it in the global config. workrex keeps the workmux names for config paths, so an existing workmux config works. See [Configuration](/guide/configuration/).

```yaml
# ~/.config/workmux/config.yaml
mode: session
```

To choose the mode for one command, pass `--mode`:

```bash
workrex add feature-branch --mode session
workrex open feature-branch --mode window
```

`--mode` overrides the config for the current command. This lets you use window mode by default but create individual worktrees as sessions when needed, or temporarily reopen a session-mode worktree as a window. `--session` is shorthand for `--mode session`.

## How it works

- **Persistence**: The mode is stored per-worktree in git config. Once a worktree is created with session mode, `open`, `close`, `remove`, and `merge` automatically use the correct mode.
- **Navigation under tmux**: `workrex add` switches your client to the new session. When a session closes, clients still viewing it return to their previous sessions. For an in-session `merge`, the merge target's managed session takes precedence when available; for `remove`, the main branch's managed session takes precedence. A configured [`default_session`](#returning-to-a-default-session) is used when no managed session is available. Clients already viewing other sessions stay put. If preferred navigation is unavailable or cannot be safely targeted, tmux chooses another session. Closing the last session detaches its clients.
- **Navigation under Rex**: `workrex add` does not move Rex.app to the new session. It prints which dashboard row to open instead. When a session that Rex.app shows closes, workrex picks the destination in the same order as under tmux. See [Rex session mode](/guide/rex/#session-mode) for the details.

## Returning to a default session

By default a client viewing a closing session returns to whichever session it was in previously. Set `default_session` to name a session to prefer instead:

```yaml
# .workmux.yaml
mode: session
default_session: main
```

`close`, `remove` and `merge` then send clients to the `main` session rather than to whatever each was viewing before. A managed session for the destination branch still wins, so this only applies where tmux would otherwise fall back to the previous session.

The value is a literal session name, so `window_prefix` is not applied and it can name a session workrex does not manage. If no session matches it exactly, clients fall back to their previous sessions as before.

## Multiple windows per session

Use the `windows` config to create multiple windows in each session. Each window can have its own pane layout. This is mutually exclusive with the top-level `panes` config.

```yaml
mode: session
windows:
  - name: editor
    panes:
      - command: <agent>
        focus: true
      - split: horizontal
        size: 20
  - name: tests
    panes:
      - command: just test --watch
  - panes:
      - command: tail -f app.log
```

Each window supports:

| Option  | Description                                               | Default      |
| ------- | --------------------------------------------------------- | ------------ |
| `name`  | Window name (if omitted, tmux names it after the command) | Auto         |
| `panes` | Pane layout (same syntax as top-level `panes`)            | Single shell |

Named windows keep their name permanently. Under tmux, unnamed windows use tmux's automatic naming based on the running command.

`focus: true` works across windows: the last pane with focus set determines which window is active when the session opens.

## Limitations

- **tmux and Rex only**: WezTerm, kitty and Zellij do not support session mode.
- **No duplicates**: Unlike window mode which supports opening multiple windows for the same worktree (with `-2`, `-3` suffixes), session mode creates one session per worktree.
