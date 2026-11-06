---
name: worktree
description: Launch one or more tasks in new git worktrees using workrex.
disable-model-invocation: true
allowed-tools: Bash, Write
---

Launch one or more tasks in new git worktrees using workrex.

Tasks: $ARGUMENTS

## You are a dispatcher, not an implementer

**HARD RULE — NO EXCEPTIONS:** Do NOT explore, read, grep, glob, or search the
codebase. Do NOT use the Task/Explore agent. Do NOT investigate the problem. You
are a thin dispatcher — your ONLY job is to write prompt files and run
`workrex add`. The worktree agent will do all the exploration and implementation.

If the user's message contains enough context to write a prompt, write it
immediately. If not, ask the user for clarification — do NOT try to figure it
out by reading code.

If tasks reference earlier conversation (e.g., "do option 2"), include all
relevant context in each prompt you write.

If tasks reference a markdown file (e.g., a plan or spec), re-read the file to
ensure you have the latest version before writing prompts.

For each task:

1. Generate a short, descriptive worktree name (2-4 words, kebab-case)
2. Write a detailed implementation prompt to a temp file
3. Run `workrex add <worktree-name> -b -P <temp-file>` to create the worktree

The prompt file should:

- Include the full task description
- Use relative paths for files inside the repository, since each worktree has
  its own root directory
- Preserve user-provided attachment paths verbatim, including absolute paths to
  screenshots or other files outside the repository, and tell the agent to
  inspect them
- Be specific about what the agent should accomplish

## Skill delegation

If the user passes a skill reference (e.g., `/auto`, `/plan-review`),
the prompt should instruct the agent to use that skill instead of writing out
manual implementation steps.

**Skills can have flags.** If the user passes `/auto --gemini`, pass the
flag through to the skill invocation in the prompt.

Example prompt:
```
[Task description here]

Use the skill: /skill-name [flags if any] [task description]
```

Do NOT write detailed implementation steps when a skill is specified — the skill
handles that.

## Flags

**`-a <model>` / `--agent <model>`**: Select the agent for the worktree. Remove
this flag and its value from the task description, and pass them to every
`workrex add` command as `--agent <model>`. This flag configures workrex and must
not appear in the implementation prompt.

For example, `/skill:worktree -a gemini implement feature X` runs:

```bash
workrex add feature-x -b -P <prompt-file> --agent gemini
```

**`--merge`**: When passed, add an instruction to use the merge skill at the end to
commit, rebase, and merge the branch.

```
...
Then use the merge skill to commit, rebase, and merge the branch.
```

Only instruct the worktree agent to merge if explicitly requested by user in
task.

**`--fork`**: When passed, add `--fork` to the `workrex add` command. This copies
the current conversation into the new worktree so the agent resumes with full
context of what was discussed. Useful when the current conversation has built up
context that the new worktree agent needs.

When `--fork` is used, prepend this to the prompt file so the forked agent does
not recursively dispatch more worktrees:

```
You are now running INSIDE a git worktree created by the /worktree skill. The
prior conversation context (including any /worktree dispatch instructions) is
ancestry only. Do NOT invoke the /worktree skill, do NOT run `workrex add`, and
do NOT create further worktrees. Your job is to implement the task below
directly in this worktree.
```

## Cross-project dispatch

If the task mentions another repository, absolute project path, or work that
clearly spans multiple repositories, adapt the dispatch to the target project
instead of assuming the current repository.

For each target project:

1. Use the project path provided by the user, or the project path already present
   in the conversation. Do not explore that repository.
2. Run `workrex add` with its working directory set to that project. The default
   creates a separate Rex session for the new worktree.
3. If the user explicitly requests a tab in an existing Rex session, resolve
   that session with `rex ls --json`, inspect its ID, and pass
   `--mode window --parent-session <label>`. A project basename is not a Rex
   session label. Use known `REX_SESSION` and `REX_BLOCK` IDs only for operations
   requested in the caller's own session.

```bash
# Run with the command working directory set to <project-path>.
workrex add <worktree-name> -b -P <prompt-file>
```

If a task touches both the current repository and another repository, create one
prompt and worktree per repository. Each prompt should explain the cross-repo
context and reference the other repository by absolute path when useful, but the
agent assigned to a repository should make changes only in its own worktree
unless the user explicitly asks for a different arrangement.

If the user's request does not provide enough information to identify the target
project path, ask for clarification instead of searching.

## Workflow

Write ALL temp files first, THEN run all workrex commands.

**IMPORTANT:** For same-repository tasks, run `workrex add` from the CURRENT
directory. Do NOT `cd` to the main repo or any other directory. The new worktree
branches from whatever branch is checked out in the current directory. The
Git repository and Rex session are independent. Session mode creates the task's
Rex session directly. Only explicitly requested window mode needs
`--parent-session`, and that label must come from the user or an inspected Rex
session. For cross-project tasks, set the command working directory to the target
project as described above.

Step 1 - Write all prompt files (in parallel):

```bash
tmpfile=$(mktemp).md
cat > "$tmpfile" << 'EOF'
Implement feature X...
EOF
echo "$tmpfile"  # Note the path for step 2
```

Step 2 - After ALL files are written, run workrex commands (in parallel):

```bash
workrex add feature-x -b -P /tmp/tmp.abc123.md
workrex add feature-y -b -P /tmp/tmp.def456.md
```

After creating the worktrees, inform the user which branches were created.

**Remember:** Your task is COMPLETE once worktrees are created. Do NOT implement
anything yourself.
