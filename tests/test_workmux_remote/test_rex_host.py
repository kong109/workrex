"""Agent terminals the loopback node hosts on its Rex server
(`terminal: rex`): one session per pane, no local relay window, the
setup token only in the agent's environment, and remove destroying the
sessions. Runs with WORKREX_TEST_NODE_TERMINAL=rex on the Rex backend,
whose isolated server stands in for the node's."""

import json
import shlex
import subprocess
import time
from pathlib import Path

import pytest

from ..conftest import get_window_name, poll_until
from ..rex_env import RexEnvironment
from ..support.bin_name import hook_bin
from .conftest import (
    LOOPBACK,
    NODE_TERMINAL,
    Node,
    commit_project,
    local_records,
    observation_path,
    wm,
    write_script,
)

TOKEN = "wr8-dummy-token-0123456789"


@pytest.fixture(autouse=True)
def rex_hosted():
    if NODE_TERMINAL != "rex":
        pytest.skip("covers WORKREX_TEST_NODE_TERMINAL=rex")


def rex_of(node: Node) -> RexEnvironment:
    assert node.rex is not None
    return node.rex


def observed_since(env, since: float) -> bool:
    """Whether a connected observation arrived after `since`."""
    path = observation_path(env)
    if not path.exists():
        return False
    observed = json.loads(path.read_text())
    return observed["connected"] and observed["last_seen"] > since


def start_workmux(env, exe: Path, repo: Path, *args: str) -> "subprocess.Popen[str]":
    """Run workmux in the background."""
    return subprocess.Popen(
        [str(exe), *args],
        cwd=repo,
        env={**env.env, **LOOPBACK},
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        text=True,
    )


def blocks(node: Node, session: str) -> list:
    view = rex_of(node).api("session.view", {"session_id": session})
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
        view = json.dumps(rex_of(node).api("session.view", {"session_id": session}))
        assert TOKEN not in view
        for block in blocks(node, session):
            assert TOKEN not in json.dumps(rex_of(node).block_call(block, "process"))
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


def test_rex_hosted_agent_gets_the_node_environment(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node, tmp_path: Path
):
    """The agent block runs with the node's environment, as a tmux session
    the node starts does, not with the environment of the Rex server."""
    env = mux_server
    node_bin = tmp_path / "node-only-bin"
    node_bin.mkdir()
    claude = write_script(
        tmp_path / "claude",
        'echo "AGENT-UP only:${WR8_NODE_ONLY:-unset}"\n'
        f'case ":$PATH:" in *":{node_bin}:"*) echo NODE-PATH;; *) echo SERVER-PATH;; esac\n'
        "exec cat > /dev/null\n",
    )
    commit_project(
        mux_repo_path,
        {"nerdfont": False, "agent": str(claude), "panes": [{"command": "<agent>"}]},
    )
    node_env = {"WR8_NODE_ONLY": "from-node", "PATH": f"{node_bin}:{env.env['PATH']}"}
    added = wm(env, workmux_exe_path, mux_repo_path, "add feat --remote lo", node_env)
    assert "Successfully created remote workspace" in added.stdout, added.stderr

    node.wait_for_agent(env, "feat", "AGENT-UP only:from-node")
    node.wait_for_agent(env, "feat", "NODE-PATH")
    wm(env, workmux_exe_path, mux_repo_path, "remove --force feat")


def test_a_renamed_hosted_session_is_still_found_by_id(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node, tmp_path: Path
):
    """Rex's Claude Code hook renames the agent's session at its first
    request. The node finds the session by the id it recorded, so the agent
    stays listed, gc keeps a spawned workspace whose agent runs there, and
    remove destroys the session."""
    env, exe, repo = mux_server, workmux_exe_path, mux_repo_path
    claude = write_script(
        tmp_path / "claude",
        f"{hook_bin()} set-window-status working </dev/null\n"
        "echo AGENT-UP\n"
        # The status record is tied to the block's foreground program.
        "while read -r line; do :; done\n",
    )
    commit_project(
        repo,
        {"nerdfont": False, "agent": str(claude), "panes": [{"command": "<agent>"}]},
    )
    added = wm(env, exe, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    node.wait_for_agent(env, "feat", "AGENT-UP")
    [record] = local_records(env)
    [session] = record["execution"]["rex"]["sessions"].values()

    def statuses() -> list:
        rows = json.loads(wm(env, exe, repo, "list --json").stdout)
        return next(row["agent_statuses"] for row in rows if row.get("remote"))

    assert poll_until(lambda: statuses() == ["working"], timeout=20), statuses()
    rex_of(node).api(
        "session.set_label", {"session_id": session, "label": "Fix the login bug"}
    )
    assert node.sessions() == []
    assert statuses() == ["working"]

    # gc goes by whether the agent runs only for a workspace an agent
    # spawned that the laptop has not adopted: here, one it forgot.
    records_dir = env.home_path / ".local/state/workmux/remote/lo/workspaces"
    local = records_dir / f"{record['ws_id']}.json"
    saved = local.read_text()
    local.unlink()
    tombstone = records_dir.parent / "forgotten" / record["ws_id"]
    tombstone.parent.mkdir(exist_ok=True)
    tombstone.write_text(str(int(time.time())))
    on_node = node.root / "state" / "workspaces" / f"{record['ws_id']}.json"
    ws = json.loads(on_node.read_text())
    ws["origin"] = {
        "parent": "w000000000000",
        "base_label": "main",
        "background": True,
        "spawned_at": int(time.time()) - 3600,
        "layout": [],
    }
    on_node.write_text(json.dumps(ws))
    gc = wm(env, exe, repo, "remote gc lo --dry-run")
    assert "Keeping workspace 'feat'" in gc.stdout, gc.stdout
    assert "Nothing to clean up on 'lo'" in gc.stdout, gc.stdout

    tombstone.unlink()
    local.write_text(saved)
    removed = wm(env, exe, repo, "remove --force feat")
    assert "Removed remote workspace 'feat'" in removed.stdout, removed.stderr
    live = [s["session_id"] for s in rex_of(node).api("session.list")["sessions"]]
    assert session not in live, live


def test_open_restarts_an_agent_whose_session_closed_since_the_observation(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node, tmp_path: Path
):
    """Closing the agent's session in Rex.app leaves the last observation
    calling it alive for up to 15 s. `open` checks the session first, so it
    restarts the agent at once instead of showing a session that is gone."""
    env, exe, repo = mux_server, workmux_exe_path, mux_repo_path
    claude = write_script(
        tmp_path / "claude", "echo AGENT-UP\nwhile read -r line; do :; done\n"
    )
    commit_project(
        repo,
        {"nerdfont": False, "agent": str(claude), "panes": [{"command": "<agent>"}]},
    )
    added = wm(env, exe, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    node.wait_for_agent(env, "feat", "AGENT-UP")
    [record] = local_records(env)
    [session] = record["execution"]["rex"]["sessions"].values()

    since = time.time()
    observer = start_workmux(env, exe, repo, "wait", "feat", "--timeout", "60")
    try:
        assert poll_until(lambda: observed_since(env, since), timeout=20)
    finally:
        observer.kill()
        observer.wait()
    rex_of(node).api("session.destroy", {"session_id": session})

    opened = wm(env, exe, repo, "open feat")
    assert "Started remote workspace 'feat'" in opened.stdout, (
        opened.stdout + opened.stderr
    )
    [record] = local_records(env)
    [restarted] = record["execution"]["rex"]["sessions"].values()
    assert restarted != session
    node.wait_for_agent(env, "feat", "AGENT-UP")
    wm(env, exe, repo, "remove --force feat")


def test_a_hosted_start_runs_four_rex_processes(
    mux_server, workmux_exe_path: Path, mux_repo_path: Path, node: Node, tmp_path: Path
):
    """A hosted start reads the server's version, status and sessions with
    one `rex` process, creates the session, swaps in the agent's block with
    one more, and types the agent's command and its Enter in one write."""
    env = mux_server
    shim = tmp_path / "rex-shim"
    shim.mkdir()
    log = tmp_path / "rex-calls"
    write_script(
        shim / "rex",
        'case "$*" in\n'
        '  *" do -e "*server.status*) step="do open";;\n'
        '  *" do -e "*session.new_split*) step="do respawn";;\n'
        '  *" do -e "*) step="do";;\n'
        "  *) step=$(printf '%s\\n' \"$*\" | awk '{ i = 1;"
        ' if ($i == "--autostart=false") i++; if ($i == "-S") i += 2;'
        ' print ($i == "api") ? $(i+2) : ($i == "block") ? $(i+4) : $i }\');;\n'
        "esac\n"
        f'printf \'%s\\t%s\\n\' "$(ps -o args= -p $PPID)" "$step" >> {shlex.quote(str(log))}\n'
        f'exec {shlex.quote(str(rex_of(node).rex))} "$@"\n',
    )
    claude = write_script(
        tmp_path / "claude", "echo AGENT-UP\nwhile read -r line; do :; done\n"
    )
    commit_project(
        mux_repo_path,
        {"nerdfont": False, "agent": str(claude), "panes": [{"command": "<agent>"}]},
    )
    added = wm(
        env,
        workmux_exe_path,
        mux_repo_path,
        "add feat --remote lo",
        {"REX_BIN_DIR": str(shim)},
    )
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    node.wait_for_agent(env, "feat", "AGENT-UP")

    calls = [line.split("\t") for line in log.read_text().splitlines()]
    start = [step for parent, step in calls if " node start-execution" in parent]
    assert start == ["do open", "session.create", "do respawn", "write"], calls
    wm(env, workmux_exe_path, mux_repo_path, "remove --force feat")
