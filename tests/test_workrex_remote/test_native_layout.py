"""The node creates one Rex session with real split, focus, and zoom state."""

from pathlib import Path

from ..conftest import poll_until
from .conftest import commit_project, local_records, wm


def layouts(node, ex_id: str):
    return [
        layer["layout"]
        for window in node.view(f"{ex_id}-0")["windows"]
        for layer in window["layers"]
    ]


def test_remote_configuration_preserves_one_session_and_split_layout(
    mux_server, workrex_exe_path: Path, mux_repo_path: Path, node
):
    commit_project(
        mux_repo_path,
        {
            "panes": [
                {"command": "echo FIRST-READY", "name": "first"},
                {
                    "command": "echo SECOND-READY",
                    "name": "second",
                    "split": "horizontal",
                    "percentage": 30,
                    "target": 0,
                },
                {
                    "command": "echo THIRD-READY",
                    "name": "third",
                    "split": "vertical",
                    "percentage": 40,
                    "target": 0,
                    "focus": True,
                    "zoom": True,
                },
            ]
        },
    )
    wm(mux_server, workrex_exe_path, mux_repo_path, "add layout --remote lo")
    workspace = node.workspace("layout")
    ex_id = workspace["execution"]["ex_id"]
    placement = workspace["execution"]["rex"]
    assert set(placement["blocks"]) == {"0", "1", "2"}
    assert len(set(placement["blocks"].values())) == 3
    assert len(set(placement["sessions"].values())) == 1
    for marker in ["FIRST-READY", "SECOND-READY", "THIRD-READY"]:
        node.wait_for_agent(mux_server, "layout", marker)
    view = node.view(f"{ex_id}-0")
    [window] = view["windows"]
    assert window["focused_block_id"] == placement["blocks"]["2"]
    assert window["zoomed_block_id"] == placement["blocks"]["2"]
    actual = layouts(node, ex_id)
    assert "split" in actual[0]
    assert {
        block["block_id"] for layer in window["layers"] for block in layer["blocks"]
    } == set(placement["blocks"].values())
    before = dict(placement["blocks"])
    wm(mux_server, workrex_exe_path, mux_repo_path, "open layout", expect_fail=True)
    assert node.workspace("layout")["execution"]["rex"]["blocks"] == before
    [after_open] = node.view(f"{ex_id}-0")["windows"]
    assert after_open["focused_block_id"] == placement["blocks"]["2"]
    attached = wm(
        mux_server,
        workrex_exe_path,
        mux_repo_path,
        f"remote attach lo {ex_id} --pane 1",
        expect_fail=True,
    )
    assert "Rex.app" in attached.stderr
    [selected] = node.view(f"{ex_id}-0")["windows"]
    assert selected["focused_block_id"] == after_open["focused_block_id"]
    wm(mux_server, workrex_exe_path, mux_repo_path, "remove --force layout")
    assert node.sessions() == [] and local_records(mux_server) == []


def test_remote_agent_uses_recorded_replacement_block_id(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    commit_project(mux_repo_path, {"panes": [{"command": "echo BLOCK-READY"}]})
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    workspace = node.workspace("feat")
    block = workspace["execution"]["rex"]["blocks"]["0"]
    node.wait_for_agent(mux_server, "feat", "BLOCK-READY")
    assert block in node.blocks(f"{workspace['execution']['ex_id']}-0")
    node.send(block, "echo FINAL-BLOCK-$((6*7))")
    assert poll_until(lambda: "FINAL-BLOCK-42" in node.capture(block), timeout=10)


def test_unconfigured_blocks_never_replace_a_closed_configured_block(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    commit_project(mux_repo_path, {"panes": [{"command": "echo CONFIGURED-READY"}]})
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    workspace = node.workspace("feat")
    placement = workspace["execution"]["rex"]
    session = placement["sessions"]["0"]
    configured = placement["blocks"]["0"]
    node.wait_for_agent(mux_server, "feat", "CONFIGURED-READY")
    user_window = node.rex.create_window(
        "user", cwd=Path(workspace["worktree_path"]), session=session
    )
    [user_block] = node.rex.pane_ids(user_window["window_id"])

    def observed_panes():
        snapshot = node.call(workrex_exe_path, "watch", {"stream": False})
        [observed] = [
            w for w in snapshot["workspaces"] if w["ws_id"] == workspace["ws_id"]
        ]
        return observed["panes"]

    before = observed_panes()
    assert [(p["index"], p["alive"]) for p in before] == [(0, True)]
    node.rex.api("block.close", {"session_id": session, "block_id": configured})
    after = observed_panes()
    assert [(p["index"], p["alive"]) for p in after] == [(0, False)]
    assert user_block in node.blocks(f"{workspace['execution']['ex_id']}-0")
    capture = node.result(
        workrex_exe_path,
        "capture",
        {"ws_id": workspace["ws_id"], "pane": 0, "lines": 20},
    )
    assert capture.returncode != 0
