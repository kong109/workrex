"""
Tests for `workrex send` command.

Tests error paths and argument validation. Happy-path tests (sending text to
a live agent pane) require a reconciled agent with matching backend/instance,
which is set up via set-window-status.
"""

from pathlib import Path
from typing import cast

from .conftest import (
    poll_until,
    run_workrex_add,
    run_workrex_command,
    write_workrex_config,
)
from .rex_env import RexEnvironment
from .support.agent_state import start_active_agent
from .support.codex import CODEX_SIMULATOR


def test_send_error_worktree_not_found(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Send fails with helpful error when worktree doesn't exist."""
    result = run_workrex_command(
        mux_server,
        workrex_exe_path,
        mux_repo_path,
        "send nonexistent hello",
        expect_fail=True,
    )
    assert result.exit_code != 0


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
    assert "No agent running" in result.stderr


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
    start_active_agent(
        env,
        workrex_exe_path,
        mux_repo_path,
        "feature-send-text",
        status="waiting",
    )

    result = run_workrex_command(
        env,
        workrex_exe_path,
        mux_repo_path,
        "send feature-send-text hello-from-send",
    )
    assert result.exit_code == 0


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
        status="waiting",
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
        status="waiting",
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
