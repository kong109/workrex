from pathlib import Path

from .conftest import poll_until, run_workrex_command, setup_git_repo
from .rex_env import RexEnvironment
from .support.agent_state import start_active_agent


def test_resurrect_restores_a_lane_named_like_the_repo_directory(
    mux_server: RexEnvironment, workrex_exe_path: Path
):
    """A lane sharing its handle with the main worktree is restored, not
    skipped as main."""
    env = mux_server
    repo = env.tmp_path / "proj"
    repo.mkdir()
    setup_git_repo(repo)
    agent = start_active_agent(env, workrex_exe_path, repo, "proj")
    env.kill_window(agent.window)
    assert poll_until(lambda: agent.window not in env.list_windows(), timeout=5.0)

    plan = run_workrex_command(env, workrex_exe_path, repo, "resurrect --dry-run")
    assert "-> restoring" in plan.stdout
    assert "skipping (main worktree)" not in plan.stdout
