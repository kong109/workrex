"""Basic worktree creation through the Rex backend."""

from pathlib import Path

import pytest

from ..conftest import (
    assert_window_exists,
    get_window_name,
    get_worktree_path,
    run_workrex_command,
    setup_git_repo,
    write_workrex_config,
)
from ..rex_env import RexEnvironment


@pytest.fixture
def repo_path(mux_server: RexEnvironment) -> Path:
    """Initialize a git repo in the test env and return its path."""
    path = mux_server.tmp_path
    setup_git_repo(path, mux_server.env)
    return path


class TestWorktreeCreation:
    """Tests for basic worktree creation with workrex add."""

    def test_add_creates_worktree(
        self, mux_server: RexEnvironment, workrex_exe_path: Path, repo_path: Path
    ):
        """workrex add should create a new git worktree."""
        branch_name = "test-feature"

        write_workrex_config(repo_path)
        run_workrex_command(
            mux_server, workrex_exe_path, repo_path, f"add {branch_name}"
        )

        expected_path = get_worktree_path(repo_path, branch_name)
        assert expected_path.exists(), f"Worktree not created at {expected_path}"
        assert (expected_path / ".git").exists(), "Worktree missing .git"

    def test_add_creates_window(
        self, mux_server: RexEnvironment, workrex_exe_path: Path, repo_path: Path
    ):
        """workrex add should create a new multiplexer window/tab."""
        branch_name = "test-feature"

        write_workrex_config(repo_path)
        run_workrex_command(
            mux_server, workrex_exe_path, repo_path, f"add {branch_name}"
        )

        expected_window = get_window_name(branch_name)
        assert_window_exists(mux_server, expected_window)
