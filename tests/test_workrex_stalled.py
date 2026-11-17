"""
Tests for an agent workrex launched that has not fired a status hook, such as
one held at a folder-trust prompt.
"""

import json
import sys
import time
from pathlib import Path

from .conftest import (
    FakeAgentInstaller,
    get_window_name,
    run_workrex_command,
    wait_for_pane_output,
    write_workrex_config,
)
from .rex_env import RexEnvironment
from .support.agent_state import get_state_dir, start_active_agent

GEMINI_HOOKS = Path(__file__).parent.parent / "resources" / "gemini" / "settings.json"

# Stands in for an agent held at a prompt before its hooks load: it asks a
# question, fires no hook, and echoes the answer.
PROMPTING_AGENT = (
    f"#!{sys.executable}\n"
    "answer = input('Trust this folder? [y/n] ')\n"
    "print(f'ANSWER {answer}')\n"
    "input()\n"
)

BRANCH = "feature-held"


def install_gemini_hooks(env: RexEnvironment) -> None:
    gemini_dir = env.home_path / ".gemini"
    gemini_dir.mkdir()
    (gemini_dir / "settings.json").write_text(GEMINI_HOOKS.read_text())


def launch_held_agent(
    env: RexEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    installer: FakeAgentInstaller,
    *,
    hooks: bool = True,
) -> str:
    if hooks:
        install_gemini_hooks(env)
    shim = installer.install("gemini", PROMPTING_AGENT)
    write_workrex_config(repo_path, panes=[{"command": "<agent>"}], agent=str(shim))
    run_workrex_command(
        env, workrex_exe_path, repo_path, f"add {BRANCH} -b -p 'fix the bug'"
    )
    window = get_window_name(BRANCH)
    wait_for_pane_output(env, window, "Trust this folder?", timeout=10.0)
    return window


def launch_records(env: RexEnvironment) -> list[Path]:
    return list((get_state_dir(env) / "launched").glob("*.json"))


def age_launch(env: RexEnvironment, seconds: int) -> None:
    """Move the launch back in time, as if the agent had sat at its prompt."""
    records = launch_records(env)
    assert len(records) == 1, records
    record = json.loads(records[0].read_text())
    record["at"] = int(time.time()) - seconds
    records[0].write_text(json.dumps(record))


def test_held_agent_can_be_captured_and_answered_without_a_hook(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    fake_agent_installer: FakeAgentInstaller,
):
    env = mux_server
    window = launch_held_agent(
        env, workrex_exe_path, mux_repo_path, fake_agent_installer
    )

    captured = run_workrex_command(
        env, workrex_exe_path, mux_repo_path, f"capture {BRANCH}"
    )
    assert "Trust this folder?" in captured.stdout

    run_workrex_command(env, workrex_exe_path, mux_repo_path, f"send {BRANCH} y")
    wait_for_pane_output(env, window, "ANSWER y", timeout=10.0)


def test_held_agent_shows_stalled_and_wait_returns_early(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    fake_agent_installer: FakeAgentInstaller,
):
    env = mux_server
    launch_held_agent(env, workrex_exe_path, mux_repo_path, fake_agent_installer)

    status = run_workrex_command(
        env, workrex_exe_path, mux_repo_path, f"status {BRANCH} --json"
    )
    [agent] = json.loads(status.stdout)["agents"]
    assert agent["status"] == "-"

    age_launch(env, 120)

    status = run_workrex_command(
        env, workrex_exe_path, mux_repo_path, f"status {BRANCH} --json"
    )
    [agent] = json.loads(status.stdout)["agents"]
    assert agent["status"] == "stalled"

    started = time.monotonic()
    waited = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        f"wait {BRANCH} --timeout 30",
        expect_fail=True,
    )
    assert time.monotonic() - started < 15
    assert waited.exit_code == 4
    assert "stalled" in waited.stderr
    assert f"capture {BRANCH}" in waited.stderr


def test_agent_without_installed_hooks_is_not_tracked_as_launched(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    fake_agent_installer: FakeAgentInstaller,
):
    """Without hooks the agent would never report, so it is never stalled."""
    env = mux_server
    launch_held_agent(
        env, workrex_exe_path, mux_repo_path, fake_agent_installer, hooks=False
    )

    assert launch_records(env) == []
    status = run_workrex_command(
        env, workrex_exe_path, mux_repo_path, f"status {BRANCH} --json"
    )
    assert json.loads(status.stdout)["agents"] == []


def test_wait_any_prefers_a_finished_agent_over_a_stalled_one(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    fake_agent_installer: FakeAgentInstaller,
):
    env = mux_server
    start_active_agent(
        env, workrex_exe_path, mux_repo_path, "feature-done", status="done"
    )
    launch_held_agent(env, workrex_exe_path, mux_repo_path, fake_agent_installer)
    age_launch(env, 120)

    waited = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        f"wait {BRANCH} feature-done --any --timeout 30",
    )
    assert waited.exit_code == 0
    assert "feature-done: done" in waited.stderr
