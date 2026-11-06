# Dynamic worktree handle completion (directory names)
# Used for open/remove/merge/path/close - repo-scoped lifecycle commands
_workrex_handles() {
    local -a handles
    handles=("${(@f)$(workrex _complete-handles 2>/dev/null)}")
    # "${(@f)...}" on empty output produces a single empty string; filter it out
    handles=(${handles:#})
    (( ${#handles} )) && compadd -a handles
}

# Dynamic agent target completion (local handles + cross-project agents)
# Used for send/capture/status/wait/run - agent communication commands
_workrex_agent_targets() {
    local -a targets
    targets=("${(@f)$(workrex _complete-agent-targets 2>/dev/null)}")
    targets=(${targets:#})
    (( ${#targets} )) && compadd -a targets
}

# Dynamic git branch completion for add command
_workrex_git_branches() {
    local -a branches
    branches=("${(@f)$(workrex _complete-git-branches 2>/dev/null)}")
    branches=(${branches:#})
    (( ${#branches} )) && compadd -a branches
}

# Main completion function.
#
# This replaces the clap-generated _workrex, wrapping _workrex_base with
# dynamic completions for positional arguments (handles, branches).
# Flag/option completion is delegated to _workrex_base which uses _arguments.
#
# Works with both autoloading (fpath) and eval:
# - Autoloaded: the file body defines all functions, then redefines _workrex
#   as this wrapper and calls it. Subsequent calls go directly to the wrapper.
# - Eval'd: all functions are defined at global scope, _workrex is registered
#   via compdef.
_workrex() {
    # Ensure standard zsh array indexing (1-based) regardless of user settings
    emulate -L zsh
    setopt extended_glob  # Required for _files glob qualifiers like *(-/)
    setopt no_nomatch     # Allow failed globs to resolve to empty list

    # Get the subcommand (second word)
    local cmd="${words[2]}"

    # List of flags that take arguments (values), by command.
    # When completing a flag value, we defer to _workrex_base so it can offer
    # file paths, custom hints, etc. via _arguments.
    # Boolean flags are excluded so we can offer positional completions after them.
    local -a arg_flags
    case "$cmd" in
        add)
            arg_flags=(
                -p --prompt
                -P --prompt-file
                --name
                -a --agent
                -n --count
                --foreach
                --branch-template
                --pr
                --forge
                # Note: --base is excluded because it needs dynamic completion
            )
            ;;
        open)
            arg_flags=(
                -p --prompt
                -P --prompt-file
                # Note: -n/--new is a boolean flag, not included here
            )
            ;;
        merge)
            arg_flags=(
                # Note: --into is excluded because it needs dynamic completion
            )
            ;;
        *)
            arg_flags=()
            ;;
    esac

    # If completing a flag (starts with -) or a flag's argument value,
    # use _workrex_base which has the full _arguments definitions.
    if [[ "${words[CURRENT]}" == -* ]] || [[ -n "${arg_flags[(r)${words[CURRENT-1]}]}" ]]; then
        _workrex_base "$@"
        return
    fi

    # For commands that take handles or branches, offer only those
    # (no file fallback from _default). Flag completion is handled above.
    case "$cmd" in
        open|remove|rm|rename|path|merge|rebase|close)
            _workrex_handles
            ;;
        send|capture|status|wait|run)
            _workrex_agent_targets
            ;;
        add)
            _workrex_git_branches
            ;;
        *)
            # For all other commands (config, sandbox, etc.), use base completions
            _workrex_base "$@"
            ;;
    esac
}

# Autoload / eval detection:
# - When autoloaded via fpath, funcstack[1] is the outer autoloaded function
#   that just defined _workrex (replacing itself). Call _workrex to handle
#   the current completion request.
# - When eval'd (e.g. eval "$(workrex completions zsh)"), we are at the top
#   level so funcstack[1] is not _workrex. Register the function with compdef.
if [ "$funcstack[1]" = "_workrex" ]; then
    _workrex "$@"
else
    compdef _workrex workrex
fi
