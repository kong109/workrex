---
title: "Changelog"
description: Release history for the Rex application
editUrl: https://github.com/kong109/workrex/edit/main/CHANGELOG.md
---

## v0.1.284-rex.1

<!-- highlight: Rex-only workflows -->

- Use Workrex naming throughout source, configuration, environment variables,
  integrations, skills, and documentation.
- Run all local and remote terminal workflows through Rex.
- Install the six bundled skills for Codex in `~/.agents/skills`.
- Group worktrees created from the same source Rex tab into one shared Rex
  session per local or remote host with `session_grouping: source_tab`.
- Accept `--remote local` in `workrex add` and add offline
  `workrex remote list [--json]` (`#82`).
- Detect remote node workspaces from the current working directory so
  `workrex remove`, `merge`, `close`, and `shell` work from remote Rex tabs
  (`#83`).
- Add `--wait`, `--until`, and `--timeout` to `workrex send`, accept pane IDs
  in `send`, `capture`, and `run`, refuse sending into `waiting` dialogs or
  ambiguous multi-agent worktrees with stable exit codes (`3` to `8`, `124`),
  and share local and remote turn fences. Ported from
  [Raine Virta](https://github.com/raine) (`#80`).
- Add GitHub Copilot CLI as a built-in agent profile (`--interactive=`,
  `--continue`, `--yolo`) and track its `waiting` state for permission prompts
  and questions. Ported from [Alexander Köpke](https://github.com/koepalex)
  (`#76`).
- Add `-b` and `--background` to `workrex open` for local and remote
  workspaces. Ported from [Raine Virta](https://github.com/raine) (`#76`).
- Allow `--pr` with `workrex add --headless` and print pull request metadata to
  stderr so `--headless --json` stdout stays valid JSON. Ported from
  [Raine Virta](https://github.com/raine) (`#76`).
- Show silent working agents in the sidebar as Quiet (`○`) with no spinner or
  elapsed timer while keeping reported status unchanged. Ported from
  [Raine Virta](https://github.com/raine) (`#78`).
- Move local edits in `workrex add -w` through the regular `add` workflow
  before hooks or agents start, keeping staged and unstaged changes separate
  with a named stash backup and leaving skipped `--patch` hunks in place.
  Ported from [dezer32](https://github.com/dezer32) and
  [Raine Virta](https://github.com/raine) (`#79`).
- Fix `workrex remove` failing on detached-HEAD worktrees or treating
  `(detached)` as a branch name. Ported from
  [Raine Virta](https://github.com/raine) (`#74`).
- Fix bare-repository `.git` pointer files and `{project}` placeholder
  expansion for bare layouts. Ported from
  [Olivier Ligot](https://github.com/oligot) and
  [Raine Virta](https://github.com/raine) (`#74`).
- Fix sandbox panics for agents without a hardcoded config mount target and
  retry transient accept errors in sandbox proxy and RPC listeners after file
  descriptor exhaustion. Ported from [Raine Virta](https://github.com/raine)
  and [Daniel Roe](https://github.com/danielroe) (`#74`).
- Fix GitLab MR checkout with newer `glab` versions by dropping deprecated
  `NO_PROMPT` configuration. Ported from
  [Joe Searle](https://github.com/jjsearle) (`#74`).
- Fix intermittent 5-second pane handshake timeouts on Rex by opening the FIFO
  read end before starting the pane shell. Ported from
  [Raine Virta](https://github.com/raine) (`#75`).
- Fix duplicate sidebar daemon starts by serializing startup with spawn and
  lifetime locks per Rex server. Ported from
  [Raine Virta](https://github.com/raine) (`#75`).
- Fix slow pane snapshot queries blocking agent registration and status updates
  in other sessions by moving live snapshots outside the agent-state lock.
  Ported from [Matt Van Horn](https://github.com/mvanhorn) (`#75`).
- Fix agent ownership in nested worktrees by matching each agent to its deepest
  containing canonical worktree. Ported from
  [Raine Virta](https://github.com/raine) (`#75`).
- Fix opening worktrees from other projects in `workrex dashboard` by resolving
  the selected worktree's repository and `.workrex.yaml` configuration and
  showing open errors in the status line. Ported from
  [Raine Virta](https://github.com/raine) (`#75`).
- Fix Pi status staying `working` after aborted turns and skip pane status
  tracking in headless, print, and RPC Pi and omp sessions. Ported from
  [Raine Virta](https://github.com/raine) (`#76`).

## v0.1.278-rex.3

The latest tagged release before the Rex-only port. See the
[release notes](https://github.com/kong109/workrex/releases/tag/v0.1.278-rex.3)
for its published artifacts and changes.

## Earlier Workrex releases

- [v0.1.278-rex.2](https://github.com/kong109/workrex/releases/tag/v0.1.278-rex.2)
- [v0.1.278-rex.1](https://github.com/kong109/workrex/releases/tag/v0.1.278-rex.1)

The original application's release history remains available in Git. Its older
terminal-specific features are not the feature reference for this Rex application.
