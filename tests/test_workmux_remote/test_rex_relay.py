"""Remote agents under the Rex backend: the relay runs in a Rex window, in a
session of its own when Rex's session default applies, over the loopback
node."""

from pathlib import Path

import pytest

from ..conftest import MuxEnvironment, get_window_name, poll_until, wait_for_pane_output
from .conftest import NODE_TERMINAL, Node, commit_project, local_records, wm
from .test_siblings import by_branch, node_pane, setup_agent_project, tell


@pytest.fixture(autouse=True)
def rex_only(mux_server: MuxEnvironment):
    if mux_server.backend_name != "rex":
        pytest.skip("covers the Rex backend")
    if NODE_TERMINAL == "rex":
        pytest.skip("covers the relay; test_rex_host covers Rex-hosted terminals")


def test_rex_relay_lifecycle_in_its_own_session(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node
):
    env = mux_server
    repo = mux_repo_path
    commit_project(
        repo, {"nerdfont": False, "panes": [{"command": "echo AGENT-READY"}]}
    )
    window = get_window_name("feat")

    added = wm(env, workmux_exe_path, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr

    [record] = local_records(env)
    assert record["window_session"] == window
    ex_id = record["execution"]["ex_id"]
    assert node.sessions() == [f"{ex_id}-0"]
    assert env.list_windows_in_session(window) == [window]
    assert window not in env.list_windows()
    wait_for_pane_output(env, window, "AGENT-READY", timeout=20)

    env.send_keys(window, "echo typed-$((40+2))")
    wait_for_pane_output(env, window, "typed-42", timeout=10)

    closed = wm(env, workmux_exe_path, repo, "close feat")
    assert "Closed the window of 'feat'" in closed.stdout, closed.stderr
    assert env._windows(window) == []
    assert poll_until(lambda: window not in env.list_sessions(), timeout=5)
    assert node.sessions() == [f"{ex_id}-0"]

    opened = wm(env, workmux_exe_path, repo, "open feat")
    assert "Attached to the running agent" in opened.stdout, opened.stderr
    assert env.list_windows_in_session(window) == [window]
    wait_for_pane_output(env, window, "typed-42", timeout=20)

    again = wm(env, workmux_exe_path, repo, "open feat")
    assert "Switched to remote workspace" in again.stdout, again.stderr
    assert len(env._windows(window)) == 1

    removed = wm(env, workmux_exe_path, repo, "remove --force feat")
    assert "Removed remote workspace 'feat'" in removed.stdout, removed.stderr
    assert env._windows(window) == []
    assert poll_until(lambda: window not in env.list_sessions(), timeout=5)
    assert node.sessions() == []
    assert local_records(env) == []


def test_rex_relay_in_window_mode_opens_beside_the_caller(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node
):
    env = mux_server
    repo = mux_repo_path
    commit_project(
        repo,
        {
            "nerdfont": False,
            "mode": "window",
            "panes": [{"command": "echo AGENT-READY"}],
        },
    )
    window = get_window_name("feat")

    added = wm(env, workmux_exe_path, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    [record] = local_records(env)
    assert record.get("window_session") is None
    assert window in env.list_windows()
    wait_for_pane_output(env, window, "AGENT-READY", timeout=20)

    wm(env, workmux_exe_path, repo, "close feat")
    assert window not in env.list_windows()
    assert "test" in env.list_sessions()

    removed = wm(env, workmux_exe_path, repo, "remove --force feat")
    assert "Removed remote workspace 'feat'" in removed.stdout, removed.stderr
    assert node.sessions() == []


def test_explicit_session_mode_is_still_refused(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node
):
    env = mux_server
    repo = mux_repo_path
    commit_project(
        repo,
        {"nerdfont": False, "mode": "session", "panes": [{"command": "true"}]},
    )
    refused = wm(env, workmux_exe_path, repo, "add feat --remote lo", expect_fail=True)
    assert "Session mode is not supported for remote agents" in refused.stderr
    assert local_records(env) == []


def test_rex_sibling_opens_in_the_parent_session(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node, tmp_path
):
    env = mux_server
    repo = mux_repo_path
    setup_agent_project(repo, tmp_path / "fake-agent")
    session = get_window_name("feat")
    sibling = get_window_name("sib")

    added = wm(env, workmux_exe_path, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    [record] = local_records(env)
    ex_id = record["execution"]["ex_id"]
    [parent] = node.workspaces()
    wait_for_pane_output(env, session, "FAKE-AGENT-READY", timeout=20)

    tell(Path(parent["worktree_path"]), "spawn sib -b -p sibling-task")
    assert poll_until(lambda: "spawn-exit-" in node_pane(node, ex_id), timeout=30), (
        node_pane(node, ex_id)
    )
    assert "spawn-exit-0" in node_pane(node, ex_id)
    sib = by_branch(node, "sib")
    assert sib is not None
    assert sib["origin"]["parent"] == parent["ws_id"]

    # The relay's observer adopts the sibling into its parent's session,
    # after the parent's window.
    assert poll_until(
        lambda: env.list_windows_in_session(session) == [session, sibling],
        timeout=20,
    ), env.list_windows_in_session(session)
    [adopted] = [r for r in local_records(env) if r["branch"] == "sib"]
    assert adopted["window_session"] == session
    wait_for_pane_output(env, sibling, "FAKE-AGENT-READY", timeout=20)

    removed = wm(env, workmux_exe_path, repo, "remove --force sib")
    assert "Removed remote workspace 'sib'" in removed.stdout, removed.stderr
    assert env.list_windows_in_session(session) == [session]
    removed = wm(env, workmux_exe_path, repo, "remove --force feat")
    assert "Removed remote workspace 'feat'" in removed.stdout, removed.stderr
    assert poll_until(lambda: session not in env.list_sessions(), timeout=5)
    assert node.sessions() == []
