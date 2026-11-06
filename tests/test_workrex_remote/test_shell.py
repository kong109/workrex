"""Native remote shell sessions and the caller's Rex application handoff."""

import os
import pty
import select
import subprocess
import time
from pathlib import Path

from ..conftest import poll_until
from .conftest import LOOPBACK, Node, agent_dirs_env, commit_project, wm


def start_shell_cli(env, exe: Path, repo: Path, handle: str = "feat"):
    """Give the CLI a real terminal while keeping every Rex target isolated."""
    master, slave = pty.openpty()
    process = subprocess.Popen(
        [str(exe), "shell", handle],
        cwd=repo,
        env={
            **env.env,
            **LOOPBACK,
            **agent_dirs_env(env.home_path),
            "REX_SERVER": env.server_url,
            "REX_SESSION": env.session_id,
            "REX_BLOCK": env.runner_block_id,
        },
        stdin=slave,
        stdout=slave,
        stderr=slave,
    )
    os.close(slave)
    return process, master


def finish_shell_cli(process, master: int, timeout: float = 20) -> str:
    chunks = []
    deadline = time.monotonic() + timeout
    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                chunks.append(data)
            elif process.poll() is not None:
                break
        process.wait(timeout=max(1, deadline - time.monotonic()))
        return b"".join(chunks).decode(errors="replace")
    finally:
        os.close(master)
        if process.poll() is None:
            process.kill()
            process.wait()


def open_shell(
    node: Node, exe: Path, workspace: dict, slot: str = "shell1", host: bool = False
) -> dict:
    return node.call(
        exe,
        "shell",
        {
            "action": "open",
            "ws_id": workspace["ws_id"],
            "ex_id": workspace["execution"]["ex_id"],
            "slot": slot,
            "host": host,
        },
    )


def close_shell(node: Node, exe: Path, workspace: dict, slot: str = "shell1") -> dict:
    return node.call(
        exe,
        "shell",
        {
            "action": "close",
            "ws_id": workspace["ws_id"],
            "ex_id": workspace["execution"]["ex_id"],
            "slot": slot,
        },
    )


def test_shell_opens_in_the_workspace_and_leaves_the_agent_running(
    mux_server, workrex_exe_path, mux_repo_path, node: Node
):
    commit_project(mux_repo_path, {"panes": [{"command": "echo AGENT-READY"}]})
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    workspace = node.workspace("feat")
    ex_id = workspace["execution"]["ex_id"]
    agent_block = node.pane(ex_id)
    opened = open_shell(node, workrex_exe_path, workspace)
    assert opened["action"] == "opened"
    assert opened["server"] == mux_server.server_url
    assert opened["session_id"].startswith("session:")
    assert opened["working_dir"] == workspace["worktree_path"]
    assert opened["container"] is False
    assert len(node.sessions()) == 2
    shell = f"{ex_id}-shell1"
    node.send(shell, 'printf "AT=%s COLOR=%s\\n" "$(pwd -P)" "$COLORTERM"')
    expected = f"AT={os.path.realpath(workspace['worktree_path'])} COLOR=truecolor"
    assert poll_until(lambda: expected in node.capture(shell), timeout=10), (
        node.capture(shell)
    )
    assert close_shell(node, workrex_exe_path, workspace) == {
        "action": "closed",
        "closed": True,
    }
    assert node.sessions() == [f"{ex_id}-0"]
    assert node.pane(ex_id) == agent_block


def test_missing_rex_app_cancels_the_unclaimed_shell(
    mux_server, workrex_exe_path, mux_repo_path, node: Node
):
    commit_project(mux_repo_path, {"panes": [{"command": "echo AGENT-READY"}]})
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    before = node.sessions()
    process, master = start_shell_cli(mux_server, workrex_exe_path, mux_repo_path)
    output = finish_shell_cli(process, master)
    assert process.returncode != 0
    assert "Rex.app" in output, output
    assert poll_until(lambda: node.sessions() == before, timeout=10), node.sessions()


def test_shell_names_what_it_cannot_open(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    wm(mux_server, workrex_exe_path, mux_repo_path, "add here")
    local = wm(
        mux_server, workrex_exe_path, mux_repo_path, "shell here", expect_fail=True
    )
    assert "'here' is a local worktree" in local.stderr
    assert 'cd "$(workrex path here)"' in local.stderr
    missing = wm(
        mux_server, workrex_exe_path, mux_repo_path, "shell nope", expect_fail=True
    )
    assert "no remote workspace named 'nope'" in missing.stderr
