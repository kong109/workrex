---
title: "send"
description: Send a prompt or instruction to a running agent
---

Sends text to the agent running in a worktree and presses Enter, as if you typed it into the agent's pane.

```bash
workrex send <name> [text]
workrex send <name> --file <file>
workrex send <name> --wait [--until <statuses>] [--timeout <secs>] [text]
```

## Arguments

- `<name>`: Worktree name, or an agent's pane ID from `workrex status --json`. Use `project:handle` to reach an agent in another project.
- `[text]`: The text to send. If you omit it, `send` reads `--file`, then stdin.

## Options

| Flag                 | Description                                                                                                                |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `-f, --file <file>`  | Read the text from a file.                                                                                                 |
| `--wait`             | Confirm the agent acted on the text, then wait for an `--until` status. An idle agent must show activity within 5 seconds. |
| `--until <statuses>` | Comma-separated statuses that end `--wait`: `working`, `waiting`, `done`. Default: `done,waiting`.                         |
| `--timeout <secs>`   | Give up `--wait` after this many seconds.                                                                                  |

`send` types nothing into an agent that is `waiting` on a dialog, because Enter would answer the dialog. It also refuses a worktree or remote workspace with more than one agent and lists their pane IDs; send to one of them by pane ID. A remote agent's pane ID is `%remote-<ws>-<pane>`, and the Rex block ID its node recorded also works.

## Exit codes

| Code  | Error                  | Meaning                                                             |
| ----- | ---------------------- | ------------------------------------------------------------------- |
| `3`   | `agent_waiting`        | The agent waits on a dialog. Nothing was sent.                      |
| `4`   | `ambiguous_target`     | The target matches several agents. Their pane IDs are listed.       |
| `5`   | `agent_not_found`      | No live agent matches the target.                                   |
| `6`   | `send_stalled`         | With `--wait`, an idle agent showed no activity within 5 seconds.   |
| `7`   | `agent_gone`           | With `--wait`, the agent exited before its turn settled.            |
| `8`   | `agent_status_unknown` | `--wait` on an agent that has reported no status. Nothing was sent. |
| `124` | `timeout`              | The `--wait --timeout` deadline passed.                             |

The error line starts with the error name, as in `Error: agent_waiting: ...`. After a `send`, [`wait`](/reference/commands/wait/) counts only the agent sent to and only states of the turn the send started.

`send` also reaches an agent that workrex launched before the agent reports any status, so you can answer a prompt that holds a `stalled` agent. It fails when the agent's pane is back at its shell, because the shell would run the text as a command.

`send` removes trailing newlines from the text. It fails when the text is empty, and when you give no text and stdin is a terminal.

For a workspace on a [remote server](/guide/remote/), `send` delivers the text to the remote agent. See [commands on remote workspaces](/guide/remote/usage/#commands-on-remote-workspaces).

`send` types single-line text into the pane. Multiline text, and all text for Codex, goes in as a paste. Under [Rex](/guide/rex/), a paste is a Rex paste event, which Rex brackets when the agent turned on bracketed paste.

## Examples

```bash
# Send an instruction to an agent
workrex send user-auth "run the tests and fix any failures"

# Send to an agent in another project
workrex send myproject:docs-update "also add the API reference"

# Send the contents of a file
workrex send user-auth --file followup.md

# Confirm the agent picked up the instruction, then wait until it finishes
workrex send --wait user-auth "fix the failing tests"

# Pipe text from another command
git diff main | workrex send review-bot
```

## See also

- [`capture`](/reference/commands/capture/) to read the agent's output
- [`wait`](/reference/commands/wait/) to block until the agent finishes
