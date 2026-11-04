---
title: "last-agent"
description: Switch back to the agent you visited last
---

Switches to the pane you were in before your last jump from `workrex dashboard` or `last-agent`. Run it again to switch back, so it toggles between two panes.

This is a hidden command (not shown in `--help`), typically bound to a key.

```bash
workrex last-agent
```

## What happens

1. Reads the pane that workrex last recorded.
2. If that pane no longer exists, prints `Last pane no longer exists` and exits.
3. If you are already in that pane, prints `Already at last agent` and exits.
4. Switches to the pane, then records the pane you came from, so the next run switches back.

If no earlier agent is recorded, it prints `No previous agent to switch to`.

## Tmux keybinding

```bash
bind Tab run-shell "workrex last-agent"
```

## Under Rex

Under [Rex](/guide/rex/), `last-agent` moves Rex.app to the target. It asks the Rex.app client that owns your pane, and it needs Remote Control, which you turn on in Rex Settings. Without it, `last-agent` prints `Rex.app did not switch: Remote Control is off. Turn it on in Rex Settings.` and then `Failed to switch to last agent`.

## Related

- [`last-done`](/reference/commands/last-done/) to jump to the agent that finished most recently
- [Status tracking](/guide/status-tracking/)
