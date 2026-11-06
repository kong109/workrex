---
title: "Session mode"
description: Organize worktrees into Rex sessions or tabs
---

Workrex creates one Rex session per worktree by default. A Rex session is a
workspace with its own tabs and terminal block layout. Use window mode when a
task should occupy a tab inside an existing session instead.

## Enabling session mode

Session mode is the default. You can make the choice explicit in `.workrex.yaml`
or `~/.config/workrex/config.yaml`:

```yaml
mode: session
```

To choose a mode for one command:

```bash
workrex add feature-branch --mode session
workrex open feature-branch --mode window
```

`--session` is shorthand for `--mode session`. Workrex stores the mode per
worktree so later `open`, `close`, `remove`, and `merge` operations address the
same kind of target. Existing worktrees keep their recorded mode.

## How it works

`add` creates the Rex session and starts its configured terminal blocks. It
leaves Rex.app showing the caller's tab, because an agent may launch the command
in the background. Open the new task from the dashboard or sidebar to show it
in the app. Jumps across tabs require Remote Control in Rex Settings.

Workrex records session IDs, so renaming a session in Rex does not lose the
worktree association. A session label, a session ID, a Git branch, and a worktree
handle are different identifiers.

## Returning to a default session

When `close`, `remove`, or `merge` closes the session Rex.app is showing, Workrex
prefers the destination branch's managed session when one is available. It then
tries `default_session`, followed by the session the worktree was added from.
An app showing another session stays where it is.

```yaml
default_session: main
```

This value is an exact Rex session label. Workrex does not prepend
`window_prefix`. Without a matching destination, Rex chooses its first session.
See [Rex session mode](/guide/rex/#session-mode) for navigation details.

## Multiple windows per session

Use `windows` to create several tabs inside each task's session. Each tab has
its own block layout. This is mutually exclusive with top-level `panes`.

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
```

`name` labels the Rex window, which Rex.app displays as a tab. Its `panes` use
the same layout syntax as top-level `panes`. The last `focus: true` block in the
configuration determines the session's active block and window.

## Window mode

For a tab in a known Rex session, set `mode: window` or pass it explicitly:

```bash
workrex add feature-branch --mode window --parent-session project-work
```

Inspect the desired session before choosing its label. Repository paths do not
identify sessions. Native Rex commands target full session, window, and block
IDs; see [Rex concepts](/guide/rex/#rex-concepts).

## Limitations

Session mode creates one session per worktree. `open --new` needs window mode,
where duplicate views use distinct managed window labels.
