"""User data that contains `other-tool` reaches workrex's errors byte-for-byte unchanged."""

import subprocess
from pathlib import Path

from .conftest import run_workrex_command, write_workrex_config
from .rex_env import RexEnvironment
from .support.bin_name import BIN


def test_branch_named_other_tool_is_kept_in_add_error(
    mux_server: RexEnvironment, workrex_exe_path: Path, repo_path: Path
):
    write_workrex_config(repo_path)
    run_workrex_command(mux_server, workrex_exe_path, repo_path, "add other-tool")
    result = run_workrex_command(
        mux_server, workrex_exe_path, repo_path, "add other-tool", expect_fail=True
    )
    assert (
        "Failed to create worktree environment for branch 'other-tool'" in result.stderr
    ), result.stderr


def test_hook_command_naming_other_tool_is_kept_in_error(
    mux_server: RexEnvironment, workrex_exe_path: Path, repo_path: Path
):
    write_workrex_config(repo_path, post_create=["false && other-tool list"])
    result = run_workrex_command(
        mux_server, workrex_exe_path, repo_path, "add hookfail", expect_fail=True
    )
    assert "'false && other-tool list'" in result.stderr, result.stderr
    assert "workrex list" not in result.stderr, result.stderr


def test_repo_path_naming_other_tool_is_kept_in_error(
    mux_server: RexEnvironment, workrex_exe_path: Path, repo_path: Path, tmp_path: Path
):
    repo = tmp_path / "my other-tool"
    subprocess.run(["git", "clone", "-q", str(repo_path), str(repo)], check=True)
    result = run_workrex_command(
        mux_server, workrex_exe_path, repo, "remove main", expect_fail=True
    )
    assert f"main worktree at '{repo.resolve()}'" in result.stderr, result.stderr


def test_hints_name_the_binary_to_type(
    mux_server: RexEnvironment, workrex_exe_path: Path, repo_path: Path
):
    result = run_workrex_command(
        mux_server, workrex_exe_path, repo_path, "open nope", expect_fail=True
    )
    assert (
        f"Use '{BIN} list' to see available worktrees." in result.stdout + result.stderr
    )


def test_init_template_names_the_binary(
    mux_server: RexEnvironment, workrex_exe_path: Path
):
    result = mux_server.run_command([str(workrex_exe_path), "init"])
    assert result.returncode == 0, result.stderr
    content = (mux_server.tmp_path / ".workrex.yaml").read_text()
    assert f'#   merge: "!{BIN} merge"' in content
    assert f"`{BIN} add -A`" in content
    assert "other-tool merge" not in content
    assert "other-tool add" not in content
