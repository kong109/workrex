---
title: "Quick start"
description: Run your first agent in its own worktree and Rex session, then merge its work
---

In this tutorial, we start an agent on a task in its own worktree and Rex
session, check on it from the dashboard, and merge its branch back.

Before you start, you need:

- workrex on your `PATH`. See [Installation](/guide/installation/).
- Rex running, with a pane open in a git repository.
- An agent CLI, such as [Claude Code](https://www.anthropic.com/claude-code).

## 1. Create the project configuration

In the repository's root, run:

```bash
workrex init
```

Workrex writes a commented `.workrex.yaml` with project defaults. See
[Configuration](/guide/configuration/) for its options.

## 2. Choose the agent

workrex starts an agent only when the project has a `CLAUDE.md` or the
configuration names an agent. Open `.workrex.yaml` and uncomment this line:

```yaml
agent: claude
```

New worktrees now open with the agent in one pane and a shell in the other.

## 3. Start a task

Create a worktree for the task, and give the agent its prompt:

```bash
workrex add user-auth -p "Add password reset to the login page"
```

workrex creates the `user-auth` branch and its worktree, opens a Rex session
for it, and starts the agent with your prompt. You should see output like this:

```
✓ Successfully created worktree and Rex session for 'user-auth'
  Base: main
  Worktree: /Users/you/projects/my-project__worktrees/user-auth
Rex.app stays put: to see user-auth, press Enter on its row in workrex dashboard.
```

The worktree is a sibling of your project directory:

```
~/projects/
├── my-project/                 your main checkout
│   └── .workrex.yaml
└── my-project__worktrees/
    └── user-auth/              the agent's checkout, on branch user-auth
```

To put worktrees somewhere else, set
[`worktree_dir`](/guide/configuration/).

## 4. Check on the agent

Open the dashboard:

```bash
workrex dashboard
```

The dashboard lists each agent with its status. Select `user-auth` and press
`Enter`. Rex.app switches to the task's session. To open the dashboard with a
key, see [Dashboard](/guide/rex/#dashboard) in the Rex guide.

## 5. Merge the work

When the agent is done, run:

```bash
workrex merge user-auth
```

workrex merges `user-auth` into `main`, then removes the worktree, the Rex
session, and the branch. `workrex list` now shows only `main`.

If you open a pull request instead, run `workrex remove user-auth` after it
merges.

## Next steps

- Run several tasks at once. Each `workrex add` gets its own worktree and
  session.
- Let an agent start tasks for you with the
  [`/worktree` skill](/guide/skills/#worktree).
- Set up panes, file copies, and setup commands in
  [Configuration](/guide/configuration/).
- See [Workflows](/guide/workflows/) and [AI agents](/guide/agents/) for
  longer patterns.
