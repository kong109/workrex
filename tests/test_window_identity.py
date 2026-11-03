"""Workmux keeps track of a window an agent renamed from inside its pane."""

import shlex
from pathlib import Path

from .conftest import (
    MuxEnvironment,
    get_window_name,
    poll_until,
    run_workmux_command,
    wait_for_window_ready,
    write_workmux_config,
)
from .rex_env import RexEnvironment


def rename_from_pane(env: MuxEnvironment, window: str, new_name: str) -> None:
    if isinstance(env, RexEnvironment):
        command = f"{shlex.quote(env.rex)} window rename {new_name}"
    else:
        command = f'tmux rename-window -t "$TMUX_PANE" {new_name}'
    env.send_keys(window, command)


def test_renamed_window_stays_listed_and_closes_on_remove(
    mux_server: MuxEnvironment, workmux_exe_path: Path, repo_path: Path
):
    env = mux_server
    branch = "feature-renamed"
    renamed = "agent-renamed"
    write_workmux_config(repo_path)
    run_workmux_command(env, workmux_exe_path, repo_path, f"add {branch} --background")
    wait_for_window_ready(env, get_window_name(branch))

    rename_from_pane(env, get_window_name(branch), renamed)
    assert poll_until(lambda: renamed in env.list_windows()), env.list_windows()

    listed = run_workmux_command(env, workmux_exe_path, repo_path, "list")
    row = next(line for line in listed.stdout.splitlines() if branch in line)
    assert "✓" in row, listed.stdout

    run_workmux_command(env, workmux_exe_path, repo_path, f"remove --force {branch}")
    assert poll_until(lambda: renamed not in env.list_windows()), env.list_windows()
