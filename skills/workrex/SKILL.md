---
name: workrex
description: Manage coding agents, git worktrees, and Rex sessions with workrex. Use when the user asks to start an agent in a project, inspect agents or their progress, or manage parallel worktree workflows in Rex.
---

# Workrex

Workrex pairs git worktrees with Rex sessions. Each task gets its own branch,
working directory, terminal blocks, and agent. It runs locally or on a configured
SSH remote. Rex is the terminal host.

## Choose the operation

- For a task that needs its own branch, use `workrex add` from the target Git
  repository. Write a self-contained prompt file and pass `-b -P <file>`.
- For a new agent in an existing checkout, use the Rex CLI to open a terminal
  running the requested agent in that directory. A worktree is optional.
- For status, output, prompts, and lifecycle operations on a managed agent, use
  Workrex handles with `status`, `capture`, `send`, `wait`, `open`, and `close`.

When asked to dispatch tasks, write the prompts from the supplied context and
run the commands. The new agents perform the code exploration and implementation.

## Rex identity and targeting

A Rex **session** is a workspace. A Rex CLI **window** is a tab in that session.
A terminal pane is a **block**. Session, window, and block IDs are distinct.

Rex sets `REX_SERVER`, `REX_SESSION`, and `REX_BLOCK` in terminal processes.
Use the inherited IDs for operations requested here or beside this agent. The
user may be viewing another session. An explicit user target takes precedence.

If an inherited ID is unavailable, check `rex whoami --json`. To find a named
session, use `rex ls --json`, then `rex session inspect <session-id> --json`.
Use returned IDs in subsequent commands. A repository directory name does not
identify a Rex session.

Native Rex commands accept options before `--`, followed by the program and
its arguments. Set `project_dir` to the known absolute project path and pass it with `-c`.

```bash
# An agent in a new tab of a known session, without moving focus.
rex window new -s "$REX_SESSION" task-agent -c "$project_dir" \
  --focus=false --json -- codex

# A separate session in an existing checkout.
rex new task-agent -c "$project_dir" --json -- codex

# A process in a new pane beside a known block.
rex split -s "$REX_SESSION" -b "$REX_BLOCK" --split=right \
  --focus=false -c "$project_dir" --json -- npm run dev
```

Save `.session_id`, `.window_id`, and `.block_ids` from the JSON results as
applicable. Creating a session does not make Rex.app display it. To show a
session on a requested client, inspect `rex client ls` and the client's
`actions`, then call its `session.select` action. Move the app only when asked.

## Create worktree agents

Workrex uses session mode by default. Run the following command with its working
directory set to the target repository:

```bash
workrex add <branch> -b -P <prompt-file>
```

This creates the branch and worktree, runs file operations and setup hooks,
creates a Rex session, and starts the configured agent in its terminal block.
`-b` keeps the caller's focus. Each prompt must include the task, relevant
constraints, and repository-relative paths. Preserve supplied attachment paths.

Useful options:

- `-a, --agent <agent>` selects the configured agent profile.
- `--base <branch>` chooses the starting branch.
- `--name <handle>` overrides the worktree directory and handle.
- `--target-name <name>` overrides the managed Rex target's label.
- `--pr <number-or-url>` checks out a pull request.
- `--continue` resumes the most recent conversation at the destination path.
- `--fork` copies a supported agent's conversation to the new worktree.
- `--open-if-exists` reopens an existing worktree instead of failing.
- `--mode window` creates a tab in a Rex session instead of a separate session.
- `--mode window --parent-session <label>` places the tab in the explicitly
  named Rex session. Inspect that session first; do not derive it from a path.
- `-W, --wait` waits until the managed terminal target closes.

Use `workrex add --help` for flags supported by the installed version. Remote
workspaces have additional constraints; check `workrex remote --help` before
combining placement, isolation, or remote flags.

## Inspect and communicate

Reuse known handles. If they are unknown, inspect the dashboard or status list.
Batch relevant status and capture calls rather than repeatedly listing everything.

```bash
workrex status
workrex status <handle-a> <handle-b>
workrex capture <handle> -n 80
workrex send <handle> "Fix the failing check and rerun it"
workrex send <handle> -f <prompt-file>
workrex wait <handle-a> <handle-b> --any --timeout 60
workrex run <handle> -- cargo test
```

`workrex wait` waits for `done` by default. Exit codes are 0 for the requested
status, 1 for timeout, 2 for an unknown worktree, and 3 for an exited agent.
An interrupted wait leaves its agents running. Resume monitoring any task you
remain responsible for.

Cross-project commands accept `project:handle` to resolve ambiguous handles.
Native `rex capture`, `rex send`, and `rex send-key` target Rex block IDs;
Workrex commands above target managed worktree handles.

## Finish or reopen work

```bash
workrex list
workrex open <handle>
workrex close <handle>
workrex merge <handle>
workrex remove <handle>
workrex rm --gone
```

`open` finds or recreates the managed Rex session or window. `close` keeps the
worktree and branch. `merge` merges and cleans up the managed terminal, worktree,
and branch unless keep behavior is selected. `remove` discards the managed
terminal and worktree without merging. Review dirty work before removal.

Rex has no session detach operation. Closing a local managed session ends its
terminal processes. To leave an agent running, select another Rex session.
`close` refuses remote workspaces; use `remove` when you intend to end them.

Use the merge skill when the agent should commit and resolve conflicts before
running `workrex merge`. For a pull request workflow, commit, push, and use the
open-pr skill, then clean up after the remote merge.

## Configuration

Global settings live in `~/.config/workrex/config.yaml`; project settings live in
`.workrex.yaml` and override global defaults. Run `workrex config reference` for
the installed version's full option reference.

```yaml
agent: codex
mode: session
window_prefix: wr-
merge_strategy: rebase
panes:
  - command: <agent>
    focus: true
  - split: horizontal
files:
  copy:
    - .env
  symlink:
    - node_modules
post_create:
  - npm install
```

Use the worktree skill to dispatch independent tasks, and the coordinator skill
when you own the full spawn, monitor, review, and merge lifecycle.
