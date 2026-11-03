"""User data that contains `workmux` reaches workrex's errors byte-for-byte unchanged."""

import subprocess
from pathlib import Path

from .support.bin_name import BIN
from .conftest import MuxEnvironment, run_workmux_command, write_workmux_config


def test_branch_named_workmux_is_kept_in_add_error(
    mux_server: MuxEnvironment, workmux_exe_path: Path, repo_path: Path
):
    write_workmux_config(repo_path)
    run_workmux_command(mux_server, workmux_exe_path, repo_path, "add workmux")
    result = run_workmux_command(
        mux_server, workmux_exe_path, repo_path, "add workmux", expect_fail=True
    )
    assert "Failed to create worktree environment for branch 'workmux'" in result.stderr, (
        result.stderr
    )


def test_hook_command_naming_workmux_is_kept_in_error(
    mux_server: MuxEnvironment, workmux_exe_path: Path, repo_path: Path
):
    write_workmux_config(repo_path, post_create=["false && workmux list"])
    result = run_workmux_command(
        mux_server, workmux_exe_path, repo_path, "add hookfail", expect_fail=True
    )
    assert "'false && workmux list'" in result.stderr, result.stderr
    assert "workrex list" not in result.stderr, result.stderr


def test_repo_path_naming_workmux_is_kept_in_error(
    mux_server: MuxEnvironment, workmux_exe_path: Path, repo_path: Path, tmp_path: Path
):
    repo = tmp_path / "my workmux"
    subprocess.run(["git", "clone", "-q", str(repo_path), str(repo)], check=True)
    result = run_workmux_command(
        mux_server, workmux_exe_path, repo, "remove main", expect_fail=True
    )
    assert f"main worktree at '{repo.resolve()}'" in result.stderr, result.stderr


def test_hints_name_the_binary_to_type(
    mux_server: MuxEnvironment, workmux_exe_path: Path, repo_path: Path
):
    result = run_workmux_command(
        mux_server, workmux_exe_path, repo_path, "open nope", expect_fail=True
    )
    assert f"Use '{BIN} list' to see available worktrees." in result.stdout + result.stderr
