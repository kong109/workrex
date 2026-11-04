"""Which local setups `add --remote` accepts: the tmux and Rex backends, with
a window or Rex's own session for the relay. Session mode, `windows:` configs
and other multiplexers are refused before anything is created."""

from pathlib import Path

from ..conftest import get_window_name
from .conftest import Node, commit_project, local_records, wm

PANES = [{"command": "true"}]


def test_other_multiplexers_are_refused(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node
):
    commit_project(mux_repo_path, {"nerdfont": False, "panes": PANES})
    refused = wm(
        mux_server,
        workmux_exe_path,
        mux_repo_path,
        "add feat --remote lo",
        extra_env={"WORKMUX_BACKEND": "zellij"},
        expect_fail=True,
    )
    assert "remote agents need tmux or Rex" in refused.stderr, refused.stderr
    assert local_records(mux_server) == []


def test_windows_config_is_refused(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node
):
    commit_project(mux_repo_path, {"nerdfont": False, "windows": [{"panes": PANES}]})
    refused = wm(
        mux_server,
        workmux_exe_path,
        mux_repo_path,
        "add feat --remote lo",
        expect_fail=True,
    )
    assert "'windows' configuration needs session mode" in refused.stderr, (
        refused.stderr
    )
    assert local_records(mux_server) == []
    assert node.workspaces() == []


def test_session_mode_is_refused(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node
):
    commit_project(mux_repo_path, {"nerdfont": False, "panes": PANES})
    flag = wm(
        mux_server,
        workmux_exe_path,
        mux_repo_path,
        "add feat --remote lo --mode session",
        expect_fail=True,
    )
    assert "Session mode is not supported for remote agents" in flag.stderr, flag.stderr

    commit_project(
        mux_repo_path, {"nerdfont": False, "mode": "session", "panes": PANES}
    )
    config = wm(
        mux_server,
        workmux_exe_path,
        mux_repo_path,
        "add feat --remote lo",
        expect_fail=True,
    )
    assert "Session mode is not supported for remote agents" in config.stderr, (
        config.stderr
    )
    assert local_records(mux_server) == []


def test_the_relay_gets_a_window_or_a_rex_session(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node
):
    commit_project(mux_repo_path, {"nerdfont": False, "panes": PANES})
    window = get_window_name("feat")
    added = wm(mux_server, workmux_exe_path, mux_repo_path, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    [record] = local_records(mux_server)
    if mux_server.backend_name == "rex":
        assert record["window_session"] == window
    else:
        assert record.get("window_session") is None
        assert window in mux_server.list_windows()
    removed = wm(mux_server, workmux_exe_path, mux_repo_path, "remove --force feat")
    assert "Removed remote workspace 'feat'" in removed.stdout, removed.stderr
