---
title: "last-done"
description: Switch to the most recently completed or waiting agent
---

Switches to the agent that most recently completed its task or is waiting for user input. Repeated invocations cycle through all done/waiting agents in reverse chronological order (most recent first). The cycle resets when a new agent finishes, so you always see the latest first.

This is a hidden command (not shown in `--help`), available from a Rex terminal or action.

It cycles through agents on the current Rex server. Use the dashboard to navigate to completed agents on a remote server.

```bash
workrex last-done
```

## How it works

1. Finds all agents with "done" or "waiting" status.
2. Sorts them by most recent status change first.
3. Switches to the most recent one.
4. On repeated invocations, advances to the next oldest agent in the list.
5. When a new agent finishes (the most recent timestamp changes), the cycle resets back to the newest.

## Under Rex

Under [Rex](/guide/rex/), `last-done` moves Rex.app to the target. It asks the Rex.app client that owns your pane, and it needs Remote Control, which you turn on in Rex Settings. Without it, `last-done` prints `Rex.app did not switch: Remote Control is off. Turn it on in Rex Settings.` once.

## Examples

```bash
# Jump to the most recently completed/waiting agent
workrex last-done

# Press again to cycle to the next oldest
workrex last-done
```

## Related

- [Status tracking](/guide/status-tracking/) explains how agent status is detected and displayed
