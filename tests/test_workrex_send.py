"""
Tests for `workrex send` command.

Tests error paths and argument validation. Happy-path tests (sending text to
a live agent pane) require a reconciled agent with matching backend/instance,
which is set up via set-window-status.
"""

import json
import time
from pathlib import Path
from typing import cast

from .conftest import (
    get_window_name,
    get_worktree_path,
    make_env_script,
    poll_until,
    run_workrex_add,
    run_workrex_command,
    wait_for_window_ready,
    write_workrex_config,
)
from .rex_env import RexEnvironment
from .support.agent_state import (
    ActiveAgent,
    build_status_cmd,
    list_agent_state_files,
    stabilize_rex_agent,
    stabilize_rex_pane,
    start_active_agent,
)
from .support.codex import CODEX_SIMULATOR
from .support.fake_agent import FAKE_AGENT


def start_agent_behind(
    env: RexEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    branch: str,
    status: str,
) -> ActiveAgent:
    command_window = env.read_current("window_id").stdout.strip()
    agent = start_active_agent(env, workrex_exe_path, repo_path, branch, status=status)
    env.select_window(command_window)
    return agent


def run_fake_agent(
    env: RexEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    branch: str,
    mode: str,
    tmp_path: Path,
) -> ActiveAgent:
    agent = start_agent_behind(env, workrex_exe_path, repo_path, branch, "done")
    script = tmp_path / "fake_agent.py"
    script.write_text(FAKE_AGENT)
    ready = tmp_path / f"{branch}-ready"
    command = f"exec python3 {script} {workrex_exe_path} {mode} {ready}"
    env.send_keys(
        agent.window,
        make_env_script(env, command, {"XDG_STATE_HOME": env.env["XDG_STATE_HOME"]}),
    )
    assert poll_until(ready.exists, timeout=10), "fake agent did not start"
    time.sleep(1)
    return agent


def pane_text(env: RexEnvironment, target: str) -> str:
    return env.capture_pane(target) or ""


def test_send_error_worktree_not_found(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    result = run_workrex_command(
        mux_server,
        workrex_exe_path,
        mux_repo_path,
        "send nonexistent hello",
        expect_fail=True,
    )
    assert result.exit_code == 5
    assert "agent_not_found" in result.stderr


def test_send_error_no_agent_in_worktree(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Send fails when worktree exists but no agent is running."""
    env = mux_server
    write_workrex_config(mux_repo_path)
    run_workrex_add(env, workrex_exe_path, mux_repo_path, "feature-no-agent")

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send feature-no-agent hello",
        expect_fail=True,
    )
    assert result.exit_code == 5
    assert "agent_not_found: No agent running" in result.stderr


def test_send_error_text_and_file_conflict(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Send fails when both text and --file are provided (clap conflict)."""
    result = run_workrex_command(
        mux_server,
        workrex_exe_path,
        mux_repo_path,
        "send some-wt hello --file /tmp/foo.txt",
        expect_fail=True,
    )
    assert result.exit_code != 0


def test_send_inline_text_to_agent(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Send delivers inline text to a running agent's pane."""
    env = mux_server
    agent = start_active_agent(
        env,
        workrex_exe_path,
        mux_repo_path,
        "feature-send-text",
        status="done",
    )

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send feature-send-text hello-from-send",
    )
    assert result.exit_code == 0
    assert poll_until(lambda: "hello-from-send" in pane_text(env, agent.window))


def test_send_refuses_an_agent_waiting_on_a_dialog(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    env = mux_server
    agent = start_active_agent(
        env,
        workrex_exe_path,
        mux_repo_path,
        "feature-send-dialog",
        status="waiting",
    )

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send feature-send-dialog must-not-answer",
        expect_fail=True,
    )
    assert result.exit_code == 3
    assert "agent_waiting" in result.stderr
    assert "must-not-answer" not in pane_text(env, agent.window)


def test_send_from_file_to_agent(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Send delivers file content to a running agent's pane."""
    env = mux_server
    start_active_agent(
        env,
        workrex_exe_path,
        mux_repo_path,
        "feature-send-file",
        status="done",
    )

    prompt_file = Path("/tmp/wr_prompt.txt")
    prompt_file.write_text("hello-from-file\n")

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        f"send feature-send-file --file {prompt_file}",
    )
    assert result.exit_code == 0


def test_send_to_a_worktree_with_several_agents_needs_a_pane_id(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    env = mux_server
    branch = "feature-send-two"
    command_window = env.read_current("window_id").stdout.strip()
    write_workrex_config(
        mux_repo_path, panes=[{"focus": True}, {"split": "horizontal"}]
    )
    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch)
    window = get_window_name(branch)
    wait_for_window_ready(env, window)
    env.select_window(command_window)
    panes = env.pane_ids(window)
    assert len(panes) == 2, panes
    for pane in panes:
        env.send_keys(pane, build_status_cmd(env, workrex_exe_path, "done"))
    assert poll_until(lambda: len(list_agent_state_files(env)) == 2)
    for pane in panes:
        stabilize_rex_pane(env, pane)

    def agents() -> list:
        listed = run_workrex_command(
            env, workrex_exe_path, mux_repo_path, f"status --json {branch}"
        )
        return json.loads(listed.stdout)["agents"]

    assert poll_until(lambda: len(agents()) == 2, timeout=10), agents()

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        f"send {branch} to-whom",
        expect_fail=True,
    )
    assert result.exit_code == 4
    assert "ambiguous_target" in result.stderr
    for pane in panes:
        assert pane in result.stderr

    second = panes[1]
    run_workrex_command(
        env, workrex_exe_path, mux_repo_path, f"send {second} to-second"
    )
    assert poll_until(lambda: "to-second" in pane_text(env, second))
    assert "to-second" not in pane_text(env, panes[0])
    captured = run_workrex_command(
        env, workrex_exe_path, mux_repo_path, f"capture {second}"
    )
    assert "to-second" in captured.stdout
    assert get_worktree_path(mux_repo_path, branch).exists()


def test_send_wait_confirms_the_turn_and_waits_for_it_to_settle(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    tmp_path: Path,
):
    env = mux_server
    run_fake_agent(
        env, workrex_exe_path, mux_repo_path, "feature-wait-turn", "turns", tmp_path
    )

    started = time.time()
    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send --wait feature-wait-turn first",
        timeout=20,
    )
    assert "feature-wait-turn: done" in result.stderr
    assert time.time() - started >= 3, "returned the previous turn's done"

    time.sleep(1)
    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send --wait --until working feature-wait-turn second",
        timeout=20,
    )
    assert "feature-wait-turn: working" in result.stderr


def test_send_then_wait_waits_for_the_turn_the_send_started(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    tmp_path: Path,
):
    env = mux_server
    run_fake_agent(
        env, workrex_exe_path, mux_repo_path, "feature-wait-fence", "turns", tmp_path
    )

    run_workrex_command(
        env, workrex_exe_path, mux_repo_path, "send feature-wait-fence task"
    )
    started = time.time()
    run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "wait feature-wait-fence --status done --timeout 20",
        timeout=25,
    )
    assert time.time() - started >= 2, "wait returned the previous turn's done"


def test_send_wait_reports_an_agent_that_never_acts(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    tmp_path: Path,
):
    env = mux_server
    run_fake_agent(
        env, workrex_exe_path, mux_repo_path, "feature-wait-stall", "silent", tmp_path
    )

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send --wait feature-wait-stall swallowed",
        expect_fail=True,
        timeout=20,
    )
    assert result.exit_code == 6
    assert "send_stalled" in result.stderr


def test_send_wait_reports_an_agent_that_exits(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    tmp_path: Path,
):
    env = mux_server
    run_fake_agent(
        env, workrex_exe_path, mux_repo_path, "feature-wait-exit", "exit", tmp_path
    )

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send --wait feature-wait-exit bye",
        expect_fail=True,
        timeout=20,
    )
    assert result.exit_code == 7
    assert "agent_gone" in result.stderr


def test_send_wait_times_out_on_a_turn_that_does_not_end(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    env = mux_server
    agent = start_agent_behind(
        env, workrex_exe_path, mux_repo_path, "feature-wait-long", "working"
    )
    stabilize_rex_agent(env, agent)

    started = time.time()
    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send --wait --timeout 2 feature-wait-long more",
        expect_fail=True,
        timeout=20,
    )
    assert result.exit_code == 124
    assert "timeout" in result.stderr
    assert time.time() - started < 10


def test_send_wait_needs_a_reported_status(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    env = mux_server
    agent = start_active_agent(
        env, workrex_exe_path, mux_repo_path, "feature-wait-unknown", status="done"
    )
    env.send_keys(agent.window, build_status_cmd(env, workrex_exe_path, "clear"))

    def statuses() -> list:
        listed = run_workrex_command(
            env, workrex_exe_path, mux_repo_path, "status --json feature-wait-unknown"
        )
        return [agent["status"] for agent in json.loads(listed.stdout)["agents"]]

    assert poll_until(lambda: statuses() == ["-"], timeout=10), statuses()

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send --wait feature-wait-unknown unobservable",
        expect_fail=True,
    )
    assert result.exit_code == 8
    assert "agent_status_unknown" in result.stderr
    assert "unobservable" not in pane_text(env, agent.window)

    run_workrex_command(
        env, workrex_exe_path, mux_repo_path, "send feature-wait-unknown hookless"
    )


def test_send_codex_prompt_uses_bracketed_paste(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    tmp_path: Path,
):
    """Codex prompts submit through bracketed paste instead of a typed burst."""
    env = cast(RexEnvironment, mux_server)
    command_window = env.read_current("window_id").stdout.strip()
    agent = start_active_agent(
        env,
        workrex_exe_path,
        mux_repo_path,
        "feature-send-codex",
        status="done",
    )
    write_workrex_config(mux_repo_path, agent="codex", panes=[{"focus": True}])

    ready_file = tmp_path / "codex-simulator-ready"
    submitted_file = tmp_path / "codex-simulator-submitted"
    input_file = tmp_path / "codex-simulator-input"
    simulator = tmp_path / "codex_simulator.py"
    simulator.write_text(CODEX_SIMULATOR)

    env.send_keys(
        agent.window,
        f"exec python3 {simulator} {workrex_exe_path} {ready_file} {submitted_file} {input_file}",
    )
    assert poll_until(ready_file.exists), "Codex simulator did not become ready"
    env.select_window(command_window)

    prompt = "continue with deterministic delivery"
    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        f"send feature-send-codex '{prompt}'",
    )

    assert result.exit_code == 0
    assert poll_until(submitted_file.exists), (
        f"prompt remained in the simulated composer; input={input_file.read_bytes()!r}"
    )
    assert submitted_file.read_text() == prompt
