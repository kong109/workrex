"""Agent terminals the loopback node hosts on its Rex server
(`terminal: rex`): one session per pane, no local relay window, the
setup token only in the agent's environment, and remove destroying the
sessions. Runs with WORKREX_TEST_NODE_TERMINAL=rex on the Rex backend,
whose isolated server stands in for the node's."""

import json
from pathlib import Path

import pytest

from ..conftest import get_window_name
from .conftest import NODE_TERMINAL, Node, commit_project, local_records, wm, write_script

TOKEN = "wr8-dummy-token-0123456789"


@pytest.fixture(autouse=True)
def rex_hosted():
    if NODE_TERMINAL != "rex":
        pytest.skip("covers WORKREX_TEST_NODE_TERMINAL=rex")


def blocks(node: Node, session: str) -> list:
    view = node.rex.api("session.view", {"session_id": session})
    return [
        block["block_id"]
        for window in view["windows"]
        for layer in window["layers"]
        for block in layer["blocks"]
    ]


def test_rex_hosted_agent_lifecycle(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node, tmp_path: Path
):
    env = mux_server
    exe = workmux_exe_path
    repo = mux_repo_path
    secrets = node.root / "secrets"
    secrets.mkdir(mode=0o700)
    (secrets / "node.json").write_text(json.dumps({"CLAUDE_CODE_OAUTH_TOKEN": TOKEN}))
    (secrets / "node.json").chmod(0o600)
    seen = tmp_path / "seen-token"
    claude = write_script(
        tmp_path / "claude",
        f'printf %s "$CLAUDE_CODE_OAUTH_TOKEN" > {seen}\n'
        'echo "AGENT-UP root:${WM_NODE_ROOT:+set}"\n'
        "exec cat > /dev/null\n",
    )
    commit_project(
        repo,
        {
            "nerdfont": False,
            "agent": str(claude),
            "panes": [
                {"command": "<agent>"},
                {"command": "echo SECOND-PANE", "split": "horizontal"},
            ],
        },
    )

    added = wm(env, exe, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    assert "Agent terminal hosted on Rex" in added.stdout, added.stdout

    [record] = local_records(env)
    ex_id = record["execution"]["ex_id"]
    placement = record["execution"]["rex"]
    assert placement["host_label"] == env.server_url
    assert node.sessions() == [f"{ex_id}-0", f"{ex_id}-1"]
    assert set(placement["sessions"].values()) == set(node.rex_sessions().values())

    # No relay: the laptop opened no window for the workspace.
    window = get_window_name("feat")
    local = [
        w["label"]
        for view in env.views()
        if not view["label"].startswith(node.tag)
        for w in view["windows"]
    ]
    assert window not in local, local

    node.wait_for_agent(env, "feat", "AGENT-UP root:set")
    node.wait_for_agent(env, "feat", "SECOND-PANE")
    assert seen.read_text() == TOKEN
    # The token reached the agent through a file it read once: it is on no
    # command line, in no screen, and no file of it is left.
    for session in node.rex_sessions().values():
        view = json.dumps(node.rex.api("session.view", {"session_id": session}))
        assert TOKEN not in view
        for block in blocks(node, session):
            assert TOKEN not in json.dumps(node.rex.block_call(block, "process"))
    for name in node.sessions():
        assert TOKEN not in node.capture(name)
    assert sorted(p.name for p in secrets.iterdir()) == ["node.json"]

    closed = wm(env, exe, repo, "close feat", expect_fail=True)
    assert "agent terminal is on Rex" in closed.stderr, closed.stderr
    assert len(node.sessions()) == 2

    # `open` shows the session on Rex.app; the test server has no app.
    opened = wm(env, exe, repo, "open feat", expect_fail=True)
    assert "Rex.app is not connected" in opened.stderr, opened.stderr

    removed = wm(env, exe, repo, "remove --force feat")
    assert "Removed remote workspace 'feat'" in removed.stdout, removed.stderr
    assert node.sessions() == []
    assert local_records(env) == []
