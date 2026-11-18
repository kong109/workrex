import json
import subprocess
from pathlib import Path

from .conftest import (
    get_worktree_path,
    run_workrex_add,
    run_workrex_command,
    run_workrex_remove,
    setup_git_repo,
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


def test_lane_named_like_the_repo_directory_is_not_the_main_worktree(
    mux_server: RexEnvironment, workrex_exe_path: Path
):
    """A lane named after the repository directory wins that name over the
    main worktree: `path` and `list` find the lane, and `rm` removes it while
    main and its branch stay."""
    env = mux_server
    repo = env.tmp_path / "proj"
    repo.mkdir()
    setup_git_repo(repo)
    write_workrex_config(repo)
    run_workrex_add(env, workrex_exe_path, repo, "proj")
    lane = get_worktree_path(repo, "proj")
    assert lane.name == repo.name

    result = run_workrex_command(env, workrex_exe_path, repo, "path proj")
    assert result.stdout.strip() == str(lane)
    listed = json.loads(
        run_workrex_command(env, workrex_exe_path, repo, "list proj --json").stdout
    )
    assert [Path(row["path"]) for row in listed] == [lane]

    run_workrex_remove(env, workrex_exe_path, repo, "proj", force=True)
    assert not lane.exists()
    assert (repo / ".git").exists()
    branches = subprocess.run(
        ["git", "branch", "--format=%(refname:short)"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    assert "main" in branches
    assert "proj" not in branches
