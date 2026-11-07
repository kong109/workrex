# Dynamic worktree handle completion (directory names)
# Used for open/remove/merge/path/close - repo-scoped lifecycle commands
function __workrex_handles
    workrex _complete-handles 2>/dev/null
end

# Dynamic agent target completion (local handles + cross-project agents)
# Used for send/capture/status/wait/run - agent communication commands
function __workrex_agent_targets
    workrex _complete-agent-targets 2>/dev/null
end

# Dynamic git branch completion for add command
function __workrex_git_branches
    workrex _complete-git-branches 2>/dev/null
end

# Lifecycle commands: local handles only
complete -c workrex -n '__fish_seen_subcommand_from open remove rm rename path merge rebase close' -f -a '(__workrex_handles)'
# Agent commands: local + cross-project targets
complete -c workrex -n '__fish_seen_subcommand_from send capture status wait run' -f -a '(__workrex_agent_targets)'
# Add command: git branches
complete -c workrex -n '__fish_seen_subcommand_from add' -f -a '(__workrex_git_branches)'
