"""Remote sessions use recorded Rex IDs independently of local window labels."""

from .conftest import commit_project, local_records, wm


def test_local_label_collision_does_not_capture_remote_ownership(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    decoy = mux_server.create_window("feat")
    commit_project(mux_repo_path, {"panes": [{"command": "echo REMOTE-READY"}]})
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    workspace = node.workspace("feat")
    session = workspace["execution"]["rex"]["sessions"]["0"]
    assert session != mux_server.session_id
    node.wait_for_agent(mux_server, "feat", "REMOTE-READY")
    wm(mux_server, workrex_exe_path, mux_repo_path, "remove --force feat")
    assert any(
        window["window_id"] == decoy["window_id"] for window in mux_server.all_windows()
    )
    assert node.sessions() == []


def test_remote_open_keeps_native_session_identity(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    commit_project(mux_repo_path, {"panes": [{"command": "echo REMOTE-READY"}]})
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    placement = local_records(mux_server)[0]["execution"]["rex"]
    before = set(placement["sessions"].values())
    [session] = before
    node.rex.api(
        "session.set_label", {"session_id": session, "label": "reviewed session"}
    )
    wm(mux_server, workrex_exe_path, mux_repo_path, "open feat", expect_fail=True)
    assert (
        set(local_records(mux_server)[0]["execution"]["rex"]["sessions"].values())
        == before
    )
    wm(mux_server, workrex_exe_path, mux_repo_path, "remove --force feat")
    assert session not in {
        entry["session_id"] for entry in node.rex.api("session.list")["sessions"]
    }
