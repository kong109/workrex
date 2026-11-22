---
title: "Changelog"
description: Release history for the Rex application
editUrl: https://github.com/kong109/workrex/edit/main/CHANGELOG.md
---

## Unreleased

<!-- highlight: Rex-only workflows -->

- Use Workrex naming throughout source, configuration, environment variables,
  integrations, skills, and documentation.
- Run all local and remote terminal workflows through Rex.
- Install the six bundled skills for Codex in `~/.agents/skills`.
- `workrex send` no longer types into an agent waiting on a dialog or guesses
  among several agents in one worktree; target an agent by pane ID instead,
  and add `--wait` to confirm the agent picked up the prompt and wait for it
  to finish. Failures exit with distinct codes, and `workrex wait` right after
  a `send` no longer returns on the previous turn's `done`. Works for remote
  agents too.

## v0.1.278-rex.3

The latest tagged release before the Rex-only port. See the
[release notes](https://github.com/kong109/workrex/releases/tag/v0.1.278-rex.3)
for its published artifacts and changes.

## Earlier Workrex releases

- [v0.1.278-rex.2](https://github.com/kong109/workrex/releases/tag/v0.1.278-rex.2)
- [v0.1.278-rex.1](https://github.com/kong109/workrex/releases/tag/v0.1.278-rex.1)

The original application's release history remains available in Git. Its older
terminal-specific features are not the feature reference for this Rex application.
