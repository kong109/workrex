"""Remote tasks always create native Rex sessions, including session mode."""

import pytest

from .conftest import commit_project, local_records, wm


@pytest.mark.parametrize("mode", [None, "session", "window"])
def test_remote_tasks_always_use_a_native_session(
    mux_server, workrex_exe_path, mux_repo_path, node, mode
):
    config = {"panes": [{"command": "echo NATIVE-READY"}]}
    if mode:
        config["mode"] = mode
    commit_project(mux_repo_path, config)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    [record] = local_records(mux_server)
    placement = record["execution"]["rex"]
    assert len(set(placement["sessions"].values())) == 1
    assert len(set(placement["blocks"].values())) == 1
    assert set(placement["sessions"].values()).isdisjoint({mux_server.session_id})
    node.wait_for_agent(mux_server, "feat", "NATIVE-READY")


def test_windows_config_is_refused_before_creation(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    commit_project(mux_repo_path, {"windows": [{"panes": [{"command": "true"}]}]})
    refused = wm(
        mux_server,
        workrex_exe_path,
        mux_repo_path,
        "add feat --remote lo",
        expect_fail=True,
    )
    assert "configure panes instead of windows" in refused.stderr
    assert local_records(mux_server) == [] and node.workspaces() == []
