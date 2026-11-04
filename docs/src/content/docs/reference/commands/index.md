---
title: "CLI reference"
description: Every workrex command, grouped as in workrex --help
---

This page lists the commands in `workrex --help`, in the same groups. For a
command's full options, run `workrex <command> --help`. "Window" means a tmux
window, a WezTerm, kitty, or Zellij tab, or a Rex window. In session mode, which
is the default under Rex, each worktree gets a session instead.
Configuration files keep their workmux names, such as `.workmux.yaml`, so
existing workmux configs work. See [Configuration](/guide/configuration/).

## Worktree lifecycle

| Command                                       | Description                                                   |
| --------------------------------------------- | ------------------------------------------------------------- |
| [`add`](/reference/commands/add/)             | Create a worktree and its window                              |
| [`remove`](/reference/commands/remove/)       | Remove a worktree, its window, and its branch without merging |
| [`rename`](/reference/commands/rename/)       | Rename a worktree, its window or session, and its branch      |
| [`merge`](/reference/commands/merge/)         | Merge a branch, then remove its worktree and window           |
| [`rebase`](/reference/commands/rebase/)       | Rebase a worktree branch onto its base branch                 |
| [`open`](/reference/commands/open/)           | Open a window for an existing worktree                        |
| [`close`](/reference/commands/close/)         | Close a worktree's window and keep the worktree and branch    |
| [`resurrect`](/reference/commands/resurrect/) | Restore worktree windows after a crash                        |

## Monitoring

| Command                                       | Description                                   |
| --------------------------------------------- | --------------------------------------------- |
| [`dashboard`](/reference/commands/dashboard/) | Show a TUI dashboard of every active agent    |
| [`sidebar`](/reference/commands/sidebar/)     | Control the live agent status sidebar in tmux |
| [`list`](/reference/commands/list/)           | List all worktrees                            |
| [`path`](/reference/commands/path/)           | Print the filesystem path of a worktree       |
| [`status`](/reference/commands/status/)       | Query agent status for worktrees              |

## Setup and configuration

| Command                                         | Description                                            |
| ----------------------------------------------- | ------------------------------------------------------ |
| [`init`](/reference/commands/init/)             | Write an example `.workmux.yaml`                       |
| [`setup`](/reference/commands/setup/)           | Set up agent status tracking hooks and install skills  |
| [`config`](/reference/commands/config/)         | Edit, locate, or print the global configuration        |
| [`sandbox`](/reference/commands/sandbox/)       | Manage sandbox settings                                |
| [`remote`](/reference/commands/remote/)         | Manage SSH remotes that run agents                     |
| [`sync-files`](/reference/commands/sync-files/) | Copy and symlink configured files into worktrees again |
| [`claude`](/reference/commands/claude/)         | Claude Code commands, such as `claude prune`           |

## Agent interaction

| Command                                           | Description                                       |
| ------------------------------------------------- | ------------------------------------------------- |
| [`send`](/reference/commands/send/)               | Send a prompt or instruction to a running agent   |
| [`capture`](/reference/commands/capture/)         | Capture terminal output from a running agent      |
| [`wait`](/reference/commands/wait/)               | Wait for agents to reach a target status          |
| [`run`](/reference/commands/run/)                 | Run a command in a worktree's window              |
| [`reap-agents`](/reference/commands/reap-agents/) | Exit tracked agent processes older than a set age |

## Help and updates

| Command                                           | Description                                     |
| ------------------------------------------------- | ----------------------------------------------- |
| [`docs`](/reference/commands/docs/)               | Show the README in the terminal                 |
| [`changelog`](/reference/commands/changelog/)     | Show the changelog (what's new in each version) |
| [`update`](/reference/commands/update/)           | Update workrex to the latest release            |
| [`completions`](/reference/commands/completions/) | Print shell completions                         |

## Commands not shown in --help

`workrex --help` does not list these commands.

| Command                                         | Description                                                        |
| ----------------------------------------------- | ------------------------------------------------------------------ |
| [`last-done`](/reference/commands/last-done/)   | Switch to the agent that finished or asked for input most recently |
| [`last-agent`](/reference/commands/last-agent/) | Switch to the last visited agent (toggle between two)              |
| [`shell`](/reference/commands/shell/)           | Open a shell in a remote workspace                                 |
| `uninstall`                                     | Remove status hooks, skills, and workrex's state and update cache  |
