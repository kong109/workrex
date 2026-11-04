---
title: "setup"
description: Install agent status tracking hooks and the bundled skills
---

Detects the agent CLIs on this machine, installs [status tracking](/guide/status-tracking/) hooks for them, and installs the bundled [skills](/guide/skills/). It shows each agent's current state and asks before it changes anything.

```bash
workrex setup [--hooks | --skills]
```

## Options

| Flag       | Description                            |
| ---------- | -------------------------------------- |
| `--hooks`  | Set up only the status tracking hooks. |
| `--skills` | Install only the skills.               |

Without a flag, `setup` does both.

## What happens

1. Detects the installed agent CLIs. If it finds none, it prints a hint and exits.
2. Lists each agent's hook status: `configured`, `update available`, `not configured` or `error`. For an update, it shows the diff.
3. Asks `Install or update status tracking hooks?` and installs the hooks for each agent that needs them.
4. Lists the bundled skills (`merge`, `rebase`, `worktree`, `coordinator`, `open-pr` and `workrex`) and the directory each agent reads skills from, then asks before it copies them.

The hooks that `setup` writes call `workrex`, and the hook files and the OpenCode plugin are named `workrex-status`, so they don't overwrite an upstream workmux install. If upstream workmux hooks are also installed, `setup` says so and leaves them in place. Remove those with upstream's `workmux uninstall`.

`setup` needs an interactive terminal. It fails when stdin is not a terminal.

## Examples

```bash
# Set up hooks and skills
workrex setup

# Update only the status tracking hooks
workrex setup --hooks

# Install only the skills
workrex setup --skills
```

## See also

- [Status tracking](/guide/status-tracking/) for what the hooks report
- [Skills](/guide/skills/) for the skill directories and manual installation
