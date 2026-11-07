"""
Tests for agent state management.

Verifies that:
1. workrex set-window-status creates agent state files
2. State files contain correct fields (pane_key, pane_pid, command, workdir, status)
3. State files contain pane identification info needed for reconciliation
4. Status updates overwrite existing state files (no duplicates)

Note: Reconciliation behavior (removing stale state files) is tested implicitly
through the dashboard TUI, which calls load_reconciled_agents(). These tests
verify the state files have the fields needed for reconciliation to work.
"""

import json
import shlex
import subprocess
import time
from pathlib import Path

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
    build_status_cmd,
    build_status_cmd_with_marker,
    get_agents_dir,
    list_agent_state_files,
    read_agent_state,
    start_active_agent,
)

# -----------------------------------------------------------------------------
# Tests
# -----------------------------------------------------------------------------


def test_set_window_status_creates_state_file(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Verifies that workrex set-window-status creates an agent state file."""
    env = mux_server
    branch_name = "feature-state-test"
    window_name = get_window_name(branch_name)

    # Configure with a pane that starts a shell (no blocking command)
    # so we can send keys to it
    write_workrex_config(
        mux_repo_path,
        panes=[
            {"focus": True},  # Just a shell, no command
        ],
    )

    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)

    # Wait for window/shell to be ready
    wait_for_window_ready(env, window_name)

    # Send set-window-status command to the pane using tab title
    # This simulates what Claude hooks do when agent starts working
    status_cmd = build_status_cmd(env, workrex_exe_path, "working")
    env.send_keys(window_name, status_cmd)

    # Wait for state file to be created
    def state_file_exists():
        files = list_agent_state_files(env)
        return len(files) > 0

    assert poll_until(state_file_exists, timeout=5.0), (
        f"No agent state file created after set-window-status. "
        f"State dir: {get_agents_dir(env)}"
    )


def test_register_agent_creates_statusless_state(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    env = mux_server
    branch_name = "feature-register-agent-test"
    window_name = get_window_name(branch_name)

    write_workrex_config(mux_repo_path, panes=[{"focus": True}])
    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)
    wait_for_window_ready(env, window_name)

    env.send_keys(window_name, build_status_cmd(env, workrex_exe_path, "working"))
    assert poll_until(lambda: len(list_agent_state_files(env)) == 1, timeout=5.0)

    register_cmd = make_env_script(
        env,
        f"{shlex.quote(str(workrex_exe_path))} register-agent",
        {"XDG_STATE_HOME": env.env["XDG_STATE_HOME"]},
    )
    registered_after = int(time.time())
    env.send_keys(window_name, register_cmd)

    def registration_is_persisted():
        files = list_agent_state_files(env)
        if len(files) != 1:
            return False
        state = read_agent_state(files[0])
        return (
            state["status"] is None
            and state["status_ts"] is None
            and state["activity_ts"] >= registered_after
        )

    assert poll_until(registration_is_persisted, timeout=5.0)


def test_set_window_status_disabled_by_env(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    env = mux_server
    branch_name = "feature-disabled-status-test"
    window_name = get_window_name(branch_name)

    write_workrex_config(
        mux_repo_path,
        panes=[
            {"focus": True},
        ],
    )

    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)
    wait_for_window_ready(env, window_name)

    marker_path = env.tmp_path / "disabled-status-finished"
    status_cmd = build_status_cmd_with_marker(
        env,
        workrex_exe_path,
        "working",
        marker_path,
        {"WORKREX_DISABLE_SET_WINDOW_STATUS": "1"},
    )
    env.send_keys(window_name, status_cmd)

    assert poll_until(lambda: marker_path.exists(), timeout=5.0)
    assert list_agent_state_files(env) == []


def test_set_window_status_accepts_explicit_rex_target(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    env = mux_server
    branch_name = "feature-status-explicit-target"
    window_name = get_window_name(branch_name)
    write_workrex_config(mux_repo_path, panes=[{"focus": True}])
    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)
    wait_for_window_ready(env, window_name)

    pane_id = env.read_blocks(window_name, ["block_id"], separator="\t").stdout.strip()
    target_env = env.env.copy()
    target_env.pop("REX_SESSION", None)
    target_env.pop("REX_BLOCK", None)
    target_env.update(
        {
            "WORKREX_STATUS_BACKEND": "rex",
            "WORKREX_STATUS_INSTANCE": env.server_url,
            "WORKREX_STATUS_PANE_ID": pane_id,
        }
    )
    result = subprocess.run(
        [str(workrex_exe_path), "set-window-status", "working"],
        cwd=get_worktree_path(mux_repo_path, branch_name),
        env=target_env,
        input="{}",
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    state = read_agent_state(list_agent_state_files(env)[0])
    assert state["pane_key"] == {
        "backend": "rex",
        "instance": env.server_url,
        "pane_id": pane_id,
    }
    assert state["status"] == "working"


def test_set_window_status_clear_clears_explicit_target_state(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """An explicit clear must reset the persisted status for exactly that pane."""
    env = mux_server
    branch_name = "feature-status-explicit-clear"
    window_name = get_window_name(branch_name)
    write_workrex_config(mux_repo_path, panes=[{"focus": True}])
    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)
    wait_for_window_ready(env, window_name)

    pane_id = env.read_blocks(window_name, ["block_id"], separator="\t").stdout.strip()
    target_env = env.env.copy()
    target_env.pop("REX_SESSION", None)
    target_env.pop("REX_BLOCK", None)
    target_env.update(
        {
            "WORKREX_STATUS_BACKEND": "rex",
            "WORKREX_STATUS_INSTANCE": env.server_url,
            "WORKREX_STATUS_PANE_ID": pane_id,
        }
    )

    def run_status(status: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [str(workrex_exe_path), "set-window-status", status],
            cwd=get_worktree_path(mux_repo_path, branch_name),
            env=target_env,
            input="{}",
            text=True,
            capture_output=True,
            check=False,
        )

    result = run_status("working")
    assert result.returncode == 0, result.stderr
    assert poll_until(lambda: len(list_agent_state_files(env)) == 1, timeout=5.0)

    before = read_agent_state(list_agent_state_files(env)[0])
    assert before["status"] == "working"
    assert before["pane_pid"] > 0

    result = run_status("clear")
    assert result.returncode == 0, result.stderr

    def status_cleared() -> bool:
        state = read_agent_state(list_agent_state_files(env)[0])
        return state["status"] is None and state["status_ts"] is None

    assert poll_until(status_cleared, timeout=5.0), "persisted status was not cleared"

    after = read_agent_state(list_agent_state_files(env)[0])
    assert after["pane_key"] == before["pane_key"]
    assert after["pane_pid"] == before["pane_pid"]
    assert after["command"] == before["command"]
    assert after["workdir"] == before["workdir"]


def test_state_file_has_correct_fields(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Verifies that agent state files contain the expected fields."""
    env = mux_server
    branch_name = "feature-fields-test"
    window_name = get_window_name(branch_name)

    write_workrex_config(
        mux_repo_path,
        panes=[
            {"focus": True},  # Just a shell
        ],
    )

    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)
    wait_for_window_ready(env, window_name)

    # Trigger state file creation using tab title
    status_cmd = build_status_cmd(env, workrex_exe_path, "working")
    env.send_keys(window_name, status_cmd)

    def state_file_exists():
        return len(list_agent_state_files(env)) > 0

    assert poll_until(state_file_exists, timeout=5.0), "State file not created"

    # Read and verify state file contents
    state_files = list_agent_state_files(env)
    state = read_agent_state(state_files[0])

    # Check required fields exist
    assert "pane_key" in state, "Missing pane_key field"
    assert "pane_pid" in state, "Missing pane_pid field"
    assert "command" in state, "Missing command field"
    assert "workdir" in state, "Missing workdir field"
    assert "status" in state, "Missing status field"

    # Check pane_key structure
    pane_key = state["pane_key"]
    assert "backend" in pane_key, "Missing backend in pane_key"
    assert "instance" in pane_key, "Missing instance in pane_key"
    assert "pane_id" in pane_key, "Missing pane_id in pane_key"

    # Verify values are sensible
    assert pane_key["backend"] == env.backend_name
    assert state["pane_pid"] > 0, "pane_pid should be positive"
    assert state["status"] == "working", (
        f"Expected status 'working', got '{state['status']}'"
    )
    # Command could be "workrex" (if captured during set-window-status) or the shell
    assert state["command"], "command should not be empty"


def test_state_file_contains_pane_info(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Verifies that state file contains valid pane identification info.

    This tests that the state file has the information needed for reconciliation
    to detect stale panes (pane_id, pane_pid, command).
    """
    env = mux_server
    branch_name = "feature-pane-info-test"
    window_name = get_window_name(branch_name)

    write_workrex_config(
        mux_repo_path,
        panes=[
            {"focus": True},  # Just a shell
        ],
    )

    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)
    wait_for_window_ready(env, window_name)

    # Create state file
    marker_path = env.tmp_path / "pane-info-status-finished"
    status_cmd = build_status_cmd_with_marker(
        env,
        workrex_exe_path,
        "working",
        marker_path,
    )
    env.send_keys(window_name, status_cmd)

    assert poll_until(lambda: marker_path.exists(), timeout=5.0)

    def state_file_exists():
        return len(list_agent_state_files(env)) > 0

    assert poll_until(state_file_exists, timeout=5.0), "State file not created"

    # Read state file and verify reconciliation-relevant fields
    state_files = list_agent_state_files(env)
    state = read_agent_state(state_files[0])

    # Verify pane_key has all required fields for pane identification
    pane_key = state["pane_key"]
    assert pane_key["pane_id"], "pane_id should be set"

    # Verify we have PID and command for stale detection
    assert state["pane_pid"] > 0, (
        "pane_pid should be positive (for PID-based stale detection)"
    )
    assert state["command"], "command should be set (for command-change detection)"

    # Verify workdir is set (useful for context)
    assert state["workdir"], "workdir should be set"


def test_same_status_update_preserves_state_for_same_process(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Repeated status updates retain the status age and stored title."""
    env = mux_server
    agent = start_active_agent(
        env,
        workrex_exe_path,
        mux_repo_path,
        "feature-same-process-state",
        status="working",
    )
    state_file = list_agent_state_files(env)[0]
    state = read_agent_state(state_file)
    state["status_ts"] = 1
    state["activity_ts"] = 2
    state["updated_ts"] = 1
    state["pane_title"] = "same-process title"
    state_file.write_text(json.dumps(state))

    marker = env.tmp_path / "same-process-status-finished"
    env.send_keys(
        agent.window,
        build_status_cmd_with_marker(env, workrex_exe_path, "working", marker),
    )
    assert poll_until(lambda: marker.exists(), timeout=5.0)

    updated = read_agent_state(state_file)
    assert updated["status_ts"] == 1
    assert updated["activity_ts"] == 2
    assert updated["pane_title"] == "same-process title"
    assert updated["updated_ts"] > 1


def test_same_status_update_uses_new_identity_after_block_recreation(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """A recreated Rex terminal block starts its own status age and title."""
    env = mux_server
    agent = start_active_agent(
        env,
        workrex_exe_path,
        mux_repo_path,
        "feature-recreated-block",
        status="working",
    )
    state_file = list_agent_state_files(env)[0]
    state = read_agent_state(state_file)
    previous_block = state["pane_key"]["pane_id"]
    state["status_ts"] = 1
    state["activity_ts"] = 1
    state["updated_ts"] = 1
    state["pane_title"] = "title from the previous block"
    state_file.write_text(json.dumps(state))

    env.kill_window(agent.window)
    run_workrex_command(env, workrex_exe_path, mux_repo_path, f"open {agent.branch}")
    wait_for_window_ready(env, agent.window)
    [current_block] = env.pane_ids(agent.window)
    assert current_block != previous_block
    marker = env.tmp_path / "new-block-status-finished"
    env.send_keys(
        current_block,
        build_status_cmd_with_marker(env, workrex_exe_path, "working", marker),
    )
    assert poll_until(lambda: marker.exists(), timeout=5.0)
    updated = next(
        read_agent_state(path)
        for path in list_agent_state_files(env)
        if read_agent_state(path)["pane_key"]["pane_id"] == current_block
    )
    assert updated["status"] == "working"
    assert updated["status_ts"] > 1
    assert updated["activity_ts"] > 1
    assert updated["pane_title"] != "title from the previous block"


def test_status_update_overwrites_state(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Verifies that calling set-window-status again updates the existing state.

    This ensures the state file is updated (not duplicated) when status changes.
    """
    env = mux_server
    branch_name = "feature-status-update-test"
    window_name = get_window_name(branch_name)

    write_workrex_config(
        mux_repo_path,
        panes=[
            {"focus": True},  # Just a shell
        ],
    )

    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)
    wait_for_window_ready(env, window_name)

    # Create initial state with "working" status
    status_cmd = build_status_cmd(env, workrex_exe_path, "working")
    env.send_keys(window_name, status_cmd)

    def state_file_exists():
        return len(list_agent_state_files(env)) > 0

    assert poll_until(state_file_exists, timeout=5.0), "State file not created"

    # Verify initial status
    state_files = list_agent_state_files(env)
    assert len(state_files) == 1, f"Expected 1 state file, got {len(state_files)}"
    state = read_agent_state(state_files[0])
    assert state["status"] == "working", f"Expected 'working', got '{state['status']}'"

    # Update to "done" status
    status_cmd = build_status_cmd(env, workrex_exe_path, "done")
    env.send_keys(window_name, status_cmd)

    # Poll for status to be updated (more reliable than fixed sleep under load)
    def status_is_done():
        files = list_agent_state_files(env)
        if not files:
            return False
        try:
            state = read_agent_state(files[0])
            return state.get("status") == "done"
        except json.JSONDecodeError:
            # File might be partially written, keep polling
            return False

    assert poll_until(status_is_done, timeout=5.0), "Status was not updated to 'done'"

    # Should still be exactly 1 state file (updated, not duplicated)
    state_files = list_agent_state_files(env)
    assert len(state_files) == 1, (
        f"Expected 1 state file after update, got {len(state_files)}"
    )


def test_set_window_status_ignores_muse_events_outside_a_turn(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    """Muse fires tool events for a `!cmd` and after a turn's Stop."""
    env = mux_server
    branch_name = "feature-status-muse-stale"
    window_name = get_window_name(branch_name)
    write_workrex_config(mux_repo_path, panes=[{"focus": True}])
    run_workrex_add(env, workrex_exe_path, mux_repo_path, branch_name)
    wait_for_window_ready(env, window_name)

    pane_id = env.read_blocks(window_name, ["block_id"], separator="\t").stdout.strip()
    target_env = env.env.copy()
    target_env.pop("REX_SESSION", None)
    target_env.pop("REX_BLOCK", None)
    target_env.update(
        {
            "WORKREX_STATUS_BACKEND": "rex",
            "WORKREX_STATUS_INSTANCE": env.server_url,
            "WORKREX_STATUS_PANE_ID": pane_id,
        }
    )
    muse_env = {**target_env, "MUSE_SESSION_ID": "s"}

    def run_status(status: str, hook: dict, run_env: dict = muse_env) -> None:
        result = subprocess.run(
            [str(workrex_exe_path), "set-window-status", status],
            cwd=get_worktree_path(mux_repo_path, branch_name),
            env=run_env,
            input=json.dumps(hook),
            text=True,
            capture_output=True,
            check=False,
        )
        assert result.returncode == 0, result.stderr

    def status() -> str | None:
        files = list_agent_state_files(env)
        return read_agent_state(files[0])["status"] if files else None

    def assert_stays(expected: str) -> None:
        time.sleep(0.5)
        assert status() == expected

    run_status("done", {"hook_event_name": "Stop", "session_id": "s"})
    assert poll_until(lambda: status() == "done", timeout=5.0)

    user_shell = {
        "hook_event_name": "PostToolUse",
        "tool_name": "shell",
        "tool_use_id": "user_shell_01a1a5bb-a828-71b0-acf7-d5b3f6964050",
        "session_id": "s",
    }
    run_status("working", user_shell)
    assert_stays("done")

    late_tool = {**user_shell, "tool_use_id": "call_01a1a5bb", "post_stop": True}
    run_status("working", late_tool)
    assert_stays("done")

    # A Notification after Stop is a pending approval.
    run_status(
        "waiting",
        {"hook_event_name": "Notification", "session_id": "s", "post_stop": True},
    )
    assert poll_until(lambda: status() == "waiting", timeout=5.0)

    run_status("working", {**late_tool, "post_stop": False})
    assert poll_until(lambda: status() == "working", timeout=5.0)

    run_status("done", {"hook_event_name": "Stop", "session_id": "s"})
    assert poll_until(lambda: status() == "done", timeout=5.0)

    # Muse's async ToolUseStart can land after Stop. It only clears waiting.
    tool_start = {"hook_event_name": "ToolUseStart", "session_id": "s"}
    run_status("working", tool_start)
    assert_stays("done")
    run_status("waiting", {"hook_event_name": "Notification", "session_id": "s"})
    assert poll_until(lambda: status() == "waiting", timeout=5.0)
    run_status("working", tool_start)
    assert poll_until(lambda: status() == "working", timeout=5.0)

    run_status("done", {"hook_event_name": "Stop", "session_id": "s"})
    assert poll_until(lambda: status() == "done", timeout=5.0)
    # Outside Muse the user_shell_ prefix means nothing.
    run_status("working", user_shell, target_env)
    assert poll_until(lambda: status() == "working", timeout=5.0)
