import json
from pathlib import Path

import pytest

from .conftest import (
    get_window_name,
    poll_until,
    run_workrex_add,
    run_workrex_command,
    wait_for_window_ready,
    write_workrex_config,
)
from .rex_env import RexEnvironment
from .support.agent_state import (
    ActiveAgent,
    build_status_cmd_with_marker,
    stabilize_rex_agent,
)


@pytest.mark.parametrize("main_agent", [False, True])
def test_nested_worktree_agent_ownership(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    main_agent: bool,
):
    env = mux_server
    repo = mux_repo_path
    runner = env.get_current_window()
    assert runner is not None
    write_workrex_config(repo, panes=[{"focus": True}])
    config = repo / ".workrex.yaml"
    config.write_text(config.read_text() + "\nworktree_dir: worktrees\n")
    run_workrex_add(env, workrex_exe_path, repo, "feature-a")
    nested_window = get_window_name("feature-a")
    wait_for_window_ready(env, nested_window)

    def register(window: str, directory: Path, status: str) -> str:
        pane = env.read_blocks(window, ["block_id"], separator="\t").stdout.strip()
        marker = env.tmp_path / f"registered-{pane.replace(':', '-')}"
        env.send_keys(window, f"cd '{directory}'")
        env.send_keys(
            window,
            build_status_cmd_with_marker(env, workrex_exe_path, status, marker),
        )
        assert poll_until(marker.exists, timeout=5)
        stabilize_rex_agent(
            env, ActiveAgent(branch="", window=window, worktree=directory)
        )
        env.select_window(runner)
        return pane

    subdir = repo / "worktrees/feature-a/src"
    subdir.mkdir()
    nested_pane = register(nested_window, subdir, "working")
    main_pane = None
    if main_agent:
        env.create_window("main-agent", cwd=repo)
        wait_for_window_ready(env, "main-agent")
        main_pane = register("main-agent", repo, "waiting")

    def command(args: str, *, fail: bool = False):
        return run_workrex_command(env, workrex_exe_path, repo, args, expect_fail=fail)

    rows = json.loads(command("list --json").stdout)
    by_branch = {row["branch"]: row for row in rows}
    assert by_branch["feature-a"]["agent_statuses"] == ["working"]
    expected_main = ["waiting"] if main_agent else []
    assert by_branch["main"]["agent_statuses"] == expected_main
    filtered = json.loads(command("list main --json").stdout)
    assert len(filtered) == 1
    assert filtered[0]["agent_statuses"] == expected_main

    status = json.loads(command("status --json").stdout)
    assert len(status["agents"]) == status["reconciled_agent_count"] == 1 + main_agent
    assert [a["pane_id"] for a in status["agents"] if a["branch"] == "feature-a"] == [
        nested_pane
    ]
    main_status = json.loads(command("status main --json").stdout)
    assert [a["pane_id"] for a in main_status["agents"]] == (
        [main_pane] if main_agent else []
    )
    capture = command("capture main", fail=not main_agent)
    if not main_agent:
        assert "No agent running" in capture.stderr
    result = command("wait main --status working --timeout 1", fail=True)
    assert result.exit_code == 1
    assert "Timeout" in result.stderr
    command("wait feature-a --status working --timeout 1")

    env.kill_window(nested_window)
    if main_agent:
        env.kill_window("main-agent")
    assert poll_until(lambda: nested_window not in env.list_windows(), timeout=5.0)
    result = command("resurrect --dry-run")
    assert "feature-a" in result.stdout
    assert "-> restoring" in result.stdout
    assert "would restore 1 worktree(s)" in result.stdout
    if main_agent:
        assert "skipping (main worktree)" in result.stdout
