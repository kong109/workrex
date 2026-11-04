"""Remote agents under the Rex backend: the relay runs in a Rex window, in a
session of its own when Rex's session default applies, over the loopback
node."""

import fcntl
import json
from pathlib import Path

import pytest

from ..conftest import MuxEnvironment, get_window_name, poll_until, wait_for_pane_output
from .conftest import Node, commit_project, local_records, wm
from .test_siblings import by_branch, node_pane, setup_agent_project, tell


@pytest.fixture(autouse=True)
def rex_only(mux_server: MuxEnvironment):
    if mux_server.backend_name != "rex":
        pytest.skip("covers the Rex backend")


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
    observer = get_window_name("other")

    # The first relay becomes the remote's observer, so the sibling is
    # adopted from the other session, not from the parent's.
    added = wm(env, workmux_exe_path, repo, "add other --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    wait_for_pane_output(env, observer, "FAKE-AGENT-READY", timeout=20)

    added = wm(env, workmux_exe_path, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    [record] = [r for r in local_records(env) if r["branch"] == "feat"]
    ex_id = record["execution"]["ex_id"]
    [parent] = [ws for ws in node.workspaces() if ws["branch"] == "feat"]
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
    assert env.list_windows_in_session(observer) == [observer]
    [adopted] = [r for r in local_records(env) if r["branch"] == "sib"]
    assert adopted["window_session"] == session
    wait_for_pane_output(env, sibling, "FAKE-AGENT-READY", timeout=20)

    removed = wm(env, workmux_exe_path, repo, "remove --force sib")
    assert "Removed remote workspace 'sib'" in removed.stdout, removed.stderr
    assert env.list_windows_in_session(session) == [session]
    for handle, window in [("feat", session), ("other", observer)]:
        removed = wm(env, workmux_exe_path, repo, f"remove --force {handle}")
        assert f"Removed remote workspace '{handle}'" in removed.stdout, removed.stderr
        assert poll_until(lambda: window not in env.list_sessions(), timeout=5)
    assert node.sessions() == []


def remote_state(env: MuxEnvironment) -> Path:
    return env.home_path / ".local/state/workmux/remote/lo"


def observer_running(env: MuxEnvironment) -> bool:
    for lock in remote_state(env).glob("observer.v*.lock"):
        with open(lock, "a") as file:
            try:
                fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return True
            fcntl.flock(file, fcntl.LOCK_UN)
    return False


def test_open_switches_to_a_window_its_own_adoption_opened(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node, tmp_path
):
    """`open` of a sibling whose window is still owed adopts it first, which
    opens the window through another backend. Even when `open` reuses its
    reads for as long as it runs, it must switch to that window, not open a
    second one."""
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
    assert by_branch(node, "sib") is not None
    assert poll_until(
        lambda: env.list_windows_in_session(session) == [session, sibling],
        timeout=20,
    ), env.list_windows_in_session(session)
    wait_for_pane_output(env, sibling, "FAKE-AGENT-READY", timeout=20)

    # No relay is left to observe the remote, and the sibling's window is
    # owed again, as when its adoption was cut short.
    closed = wm(env, workmux_exe_path, repo, "close feat")
    assert "Closed the window of 'feat'" in closed.stdout, closed.stderr
    env.kill_window(sibling)
    assert poll_until(lambda: not observer_running(env), timeout=10)
    [(path, owed)] = [
        (path, json.loads(path.read_text()))
        for path in (remote_state(env) / "workspaces").glob("*.json")
        if json.loads(path.read_text())["branch"] == "sib"
    ]
    owed["window_pending"] = True
    path.write_text(json.dumps(owed))
    assert env._windows(sibling) == []

    opened = wm(
        env,
        workmux_exe_path,
        repo,
        "open sib",
        extra_env={"WORKMUX_REX_READ_REUSE_MS": "600000"},
    )
    assert "Switched to remote workspace 'sib'" in opened.stdout, opened.stdout
    assert env.list_windows_in_session(session) == [sibling]

    for handle in ["sib", "feat"]:
        removed = wm(env, workmux_exe_path, repo, f"remove --force {handle}")
        assert f"Removed remote workspace '{handle}'" in removed.stdout, removed.stderr
    assert node.sessions() == []
