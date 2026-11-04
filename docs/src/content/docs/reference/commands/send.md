---
title: "send"
description: Send a prompt or instruction to a running agent
---

Sends text to the agent running in a worktree and presses Enter, as if you typed it into the agent's pane.

```bash
workrex send <name> [text]
workrex send <name> --file <file>
```

## Arguments

- `<name>`: Worktree name. Use `project:handle` to reach an agent in another project.
- `[text]`: The text to send. If you omit it, `send` reads `--file`, then stdin.

## Options

| Flag                | Description                |
| ------------------- | -------------------------- |
| `-f, --file <file>` | Read the text from a file. |

`send` removes trailing newlines from the text. It fails when the text is empty, and when you give no text and stdin is a terminal.

For a workspace on a [remote server](/guide/remote/), `send` delivers the text to the remote agent. See [commands on remote workspaces](/guide/remote/usage/#commands-on-remote-workspaces).

`send` types single-line text into the pane. Multiline text, and all text for Codex, goes in as a paste. Under [Rex](/guide/rex/), a paste is a Rex paste event, which Rex brackets when the agent turned on bracketed paste. tmux pastes through a paste buffer.

## Examples

```bash
# Send an instruction to an agent
workrex send user-auth "run the tests and fix any failures"

# Send to an agent in another project
workrex send myproject:docs-update "also add the API reference"

# Send the contents of a file
workrex send user-auth --file followup.md

# Pipe text from another command
git diff main | workrex send review-bot
```

## See also

- [`capture`](/reference/commands/capture/) to read the agent's output
- [`wait`](/reference/commands/wait/) to block until the agent finishes
