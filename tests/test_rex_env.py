"""The Rex test environment cleans up the server it starts."""

from pathlib import Path

import pytest

from .rex_env import RexEnvironment, rex_binary


@pytest.mark.skipif(rex_binary() is None, reason="rex CLI not found")
def test_a_failed_start_stops_the_server(tmp_path: Path, monkeypatch):
    env = RexEnvironment(tmp_path)
    launched = []

    def failing_api(self, method, args):
        launched.append(self.server)
        raise RuntimeError(f"{method} failed")

    monkeypatch.setattr(RexEnvironment, "api", failing_api)
    try:
        with pytest.raises(RuntimeError):
            env.start_server()
        assert launched[0].poll() is not None
    finally:
        if launched and launched[0].poll() is None:
            launched[0].kill()
            launched[0].wait(timeout=5)


def test_native_queries_keep_window_and_block_identity_after_relabel(
    mux_server: RexEnvironment,
):
    env = mux_server
    initial = env.window_view("test")
    window_id = initial["window_id"]
    block_ids = env.pane_ids(window_id)
    assert len(block_ids) == 1
    assert block_ids[0].startswith("block:")

    env.rename_window(window_id, "repeated-label")
    created = env.create_window("repeated-label")
    assert created["window_id"] != window_id
    assert env.window_view(window_id)["label"] == "repeated-label"
    assert env.pane_ids(window_id) == block_ids
    rows = env.read_windows(
        ["window_id", "label"], all_sessions=True
    ).stdout.splitlines()
    assert f"{window_id}\trepeated-label" in rows
    assert f"{created['window_id']}\trepeated-label" in rows

    env.kill_window(window_id)
    assert created["window_id"] in env.list_window_ids()
    assert window_id not in env.list_window_ids()


def test_native_session_queries_and_renames_preserve_session_identity(
    mux_server: RexEnvironment,
):
    env = mux_server
    env.new_session("task-session")
    session_id = env.session_view("task-session")["session_id"]
    env.rename_session(session_id, "renamed-task")

    assert env.session_view("renamed-task")["session_id"] == session_id
    assert env.session_result(session_id).returncode == 0
    assert env.session_result("task-session").returncode != 0
    assert session_id in env.read_sessions("session_id").stdout.splitlines()
