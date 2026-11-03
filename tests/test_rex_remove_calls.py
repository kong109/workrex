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

# `rm` makes 11 calls: server.version, session.view (one per backend, and
# `rm` builds two), session.list, target.resolve, process and title of the
# caller's pane, close_window, and session.view, session.list and process to
# confirm the window closed. The test sets a reuse window longer than the
# command, so machine load cannot expire a read and add a call. Before reads
# were reused, `rm` made 23.
REMOVE_CALLS = 11


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
        pre_run_env={
            "REX_BIN_DIR": str(shim_dir),
            "WORKMUX_REX_READ_REUSE_MS": "600000",
        },
    )
    assert poll_until(lambda: get_window_name(branch) not in env.list_windows())
    calls = log.read_text().splitlines()
    assert len(calls) == REMOVE_CALLS, "\n".join(calls)
