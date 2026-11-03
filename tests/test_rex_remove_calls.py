"""Removing a worktree on Rex reuses the session reads it repeats."""

import shlex
from pathlib import Path

import pytest

from .conftest import (
    MuxEnvironment,
    get_window_name,
    poll_until,
    run_workmux_command,
    wait_for_window_ready,
    write_workmux_config,
)
from .rex_env import RexEnvironment

# At fix7, `rm` made 11 calls: server.version, then session.view twice
# (git work between them outlasts the reuse window), session.list,
# target.resolve, process and title of the caller's pane, close_window, and
# session.view, session.list and process to confirm the window closed.
# Before reads were reused it made 23: 13 session views and 4 session lists.
MAX_REMOVE_CALLS = 11


def test_remove_reuses_repeated_session_reads(
    mux_server: MuxEnvironment, workmux_exe_path: Path, repo_path: Path, tmp_path: Path
):
    env = mux_server
    if not isinstance(env, RexEnvironment):
        pytest.skip("counts rex CLI calls")
    branch = "feature-counted"
    write_workmux_config(repo_path)
    run_workmux_command(env, workmux_exe_path, repo_path, f"add {branch} --background")
    wait_for_window_ready(env, get_window_name(branch))

    shim_dir = tmp_path / "rex-shim"
    shim_dir.mkdir()
    log = tmp_path / "rex-calls.log"
    shim = shim_dir / "rex"
    shim.write_text(
        f'#!/bin/sh\necho "$*" >> {shlex.quote(str(log))}\n'
        f'exec {shlex.quote(env.rex)} "$@"\n'
    )
    shim.chmod(0o755)

    run_workmux_command(
        env,
        workmux_exe_path,
        repo_path,
        f"remove --force {branch}",
        pre_run_env={"REX_BIN_DIR": str(shim_dir)},
    )
    assert poll_until(lambda: get_window_name(branch) not in env.list_windows())
    calls = log.read_text().splitlines()
    assert len(calls) <= MAX_REMOVE_CALLS, "\n".join(calls)
