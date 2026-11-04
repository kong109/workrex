---
title: "docs"
description: Show the README of the installed workrex in the terminal
---

Shows the README that was built into the installed `workrex` binary, so it
always matches the version you run.

```bash
workrex docs
```

When standard output is a terminal, `workrex docs` renders the Markdown with
colors and opens it in `$PAGER`, or `less -R` when `$PAGER` is not set. When
output goes to a pipe or a file, it prints the raw Markdown, which suits an
agent's context.

## Use with an agent

To have an agent configure workrex for you, ask it to read the docs first:

```text
> run `workrex docs` and write a .workmux.yaml with the agent in the left pane,
  and neovim above an empty shell on the right
```

The agent reads the raw Markdown from the command's output and writes the
project's `.workmux.yaml`.
