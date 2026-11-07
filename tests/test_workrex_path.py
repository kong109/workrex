from pathlib import Path

from .conftest import (
    get_worktree_path,
    run_workrex_add,
    run_workrex_command,
    write_workrex_config,
)
from .rex_env import RexEnvironment


def test_path_returns_worktree_path(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Verifies `workrex path` returns the correct path for an existing worktree."""
    env = mux_server
    branch_name = "feature-test"
    write_workrex_config(mux_repo_path)
    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)

    result = run_workrex_command(
        env, workrex_exe_path, mux_repo_path, f"path {branch_name}"
    )

    expected_path = get_worktree_path(mux_repo_path, branch_name)
    assert result.stdout.strip() == str(expected_path)


def test_path_fails_for_nonexistent_worktree(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Verifies `workrex path` fails with non-zero exit code for nonexistent worktree."""
    env = mux_server

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "path nonexistent-branch",
        expect_fail=True,
    )

    assert result.exit_code != 0
    assert "not found" in result.stderr.lower() or "worktree" in result.stderr.lower()
