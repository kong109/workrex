"""Recorded Rex shell identity, workspace environment, and canceled opens."""

import json
import signal
import subprocess
import sys

import pytest
import yaml

from ..conftest import poll_until
from .conftest import LOOPBACK, agent_dirs_env, commit_project, wm
from .test_shell import close_shell, finish_shell_cli, open_shell, start_shell_cli


def setup(env, exe, repo, node, *, nested=False):
    directory = repo / "app" if nested else repo
    directory.mkdir(exist_ok=True)
    agent = env.home_path / "claude"
    agent.write_text("#!/bin/sh\necho AGENT-READY\nexec cat >/dev/null\n")
    agent.chmod(0o755)
    commit_project(directory, {"agent": str(agent), "panes": [{"command": "<agent>"}]})
    wm(env, exe, directory, "add feat --remote lo")
    return node.workspace("feat")


def test_host_shell_receives_workspace_environment(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    workspace = setup(mux_server, workrex_exe_path, mux_repo_path, node)
    secrets = node.root / "secrets"
    secrets.mkdir(exist_ok=True)
    (secrets / "node.json").write_text(
        json.dumps({"CLAUDE_CODE_OAUTH_TOKEN": "review-token-value"})
    )
    open_shell(node, workrex_exe_path, workspace)
    shell = f"{workspace['execution']['ex_id']}-shell1"
    node.send(shell, 'printf "TOKEN=%s\\n" "${CLAUDE_CODE_OAUTH_TOKEN:-missing}"')
    assert poll_until(
        lambda: "TOKEN=review-token-value" in node.capture(shell), timeout=10
    )
    for session in node.rex_sessions().values():
        assert "review-token-value" not in json.dumps(
            node.rex.api("session.view", {"session_id": session})
        )


def test_same_shell_slot_reuses_its_live_session(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    workspace = setup(mux_server, workrex_exe_path, mux_repo_path, node)
    first = open_shell(node, workrex_exe_path, workspace)
    shell = f"{workspace['execution']['ex_id']}-shell1"
    node.send(shell, "export REVIEW_VALUE=retained; echo SET-$REVIEW_VALUE")
    assert poll_until(lambda: "SET-retained" in node.capture(shell), timeout=10)
    second = open_shell(node, workrex_exe_path, workspace)
    assert first["session_id"] == second["session_id"]
    assert len(node.sessions()) == 2
    node.send(shell, 'printf "AFTER-%s\\n" "$REVIEW_VALUE"')
    assert poll_until(lambda: "AFTER-retained" in node.capture(shell), timeout=10)
    gc = wm(mux_server, workrex_exe_path, mux_repo_path, "remote gc lo --dry-run")
    assert f"Rex session {shell}" not in gc.stdout


def test_multiple_shells_are_independent(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    workspace = setup(mux_server, workrex_exe_path, mux_repo_path, node)
    first = open_shell(node, workrex_exe_path, workspace, "shell1")
    second = open_shell(node, workrex_exe_path, workspace, "shell2")
    assert first["session_id"] != second["session_id"]
    close_shell(node, workrex_exe_path, workspace, "shell1")
    assert second["session_id"] in node.rex_sessions().values()
    shell = f"{workspace['execution']['ex_id']}-shell2"
    node.send(shell, "echo STILL-$((6*7))")
    assert poll_until(lambda: "STILL-42" in node.capture(shell), timeout=10)
    wm(mux_server, workrex_exe_path, mux_repo_path, "remove --force feat")
    assert node.sessions() == []


def test_closing_a_renamed_shell_uses_recorded_session_id(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    workspace = setup(mux_server, workrex_exe_path, mux_repo_path, node)
    opened = open_shell(node, workrex_exe_path, workspace)
    node.rex.api(
        "session.set_label",
        {"session_id": opened["session_id"], "label": "user renamed shell"},
    )
    decoy = node.plant_session(f"{workspace['execution']['ex_id']}-shell1")
    close_shell(node, workrex_exe_path, workspace)
    live = {s["session_id"] for s in node.rex.api("session.list")["sessions"]}
    assert opened["session_id"] not in live
    assert decoy in live


def test_nested_shell_directory(mux_server, workrex_exe_path, mux_repo_path, node):
    workspace = setup(mux_server, workrex_exe_path, mux_repo_path, node, nested=True)
    opened = open_shell(node, workrex_exe_path, workspace)
    assert opened["working_dir"].endswith("/app")
    shell = f"{workspace['execution']['ex_id']}-shell1"
    node.send(shell, 'printf "CWD=%s\\n" "$(pwd -P)"')
    assert poll_until(
        lambda: "CWD=" + opened["working_dir"] in node.capture(shell), timeout=10
    )


def test_noninteractive_refuses_before_creation(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    setup(mux_server, workrex_exe_path, mux_repo_path, node)
    before = node.sessions()
    result = subprocess.run(
        [str(workrex_exe_path), "shell", "feat"],
        cwd=mux_repo_path,
        env={**mux_server.env, **LOOPBACK, **agent_dirs_env(mux_server.home_path)},
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0 and "needs a terminal" in result.stderr
    assert node.sessions() == before


def test_shell_close_cannot_kill_agent(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    workspace = setup(mux_server, workrex_exe_path, mux_repo_path, node)
    result = node.result(
        workrex_exe_path,
        "shell",
        {
            "action": "close",
            "ws_id": workspace["ws_id"],
            "ex_id": workspace["execution"]["ex_id"],
            "slot": "0",
        },
    )
    assert (
        result.returncode != 0
        and "not a shell session" in result.stdout + result.stderr
    )
    assert len(node.sessions()) == 1


def test_cancel_before_open_prevents_late_creation(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    workspace = setup(mux_server, workrex_exe_path, mux_repo_path, node)
    assert close_shell(node, workrex_exe_path, workspace) == {
        "action": "closed",
        "closed": False,
    }
    result = node.result(
        workrex_exe_path,
        "shell",
        {
            "action": "open",
            "ws_id": workspace["ws_id"],
            "ex_id": workspace["execution"]["ex_id"],
            "slot": "shell1",
        },
    )
    assert result.returncode != 0 and "canceled" in result.stdout + result.stderr
    assert len(node.sessions()) == 1


@pytest.mark.parametrize("response", ["delayed", "lost"])
def test_failed_open_response_leaves_no_shell(
    mux_server, workrex_exe_path, mux_repo_path, node, response
):
    wrapper = mux_server.home_path / "delayed-shell-node"
    wrapper.write_text(
        f"#!{sys.executable}\n"
        "import json, os, subprocess, sys, time\n"
        f"binary = {str(workrex_exe_path)!r}\n"
        "if sys.argv[1:3] != ['node', 'shell']: os.execv(binary, [binary, *sys.argv[1:]])\n"
        "raw = sys.stdin.buffer.read()\n"
        "result = subprocess.run([binary, *sys.argv[1:]], input=raw, capture_output=True)\n"
        "if json.loads(raw).get('action') == 'open':\n"
        f"    if {response!r} == 'lost': sys.exit(75)\n"
        "    time.sleep(3)\n"
        "sys.stdout.buffer.write(result.stdout); sys.stderr.buffer.write(result.stderr)\n"
        "sys.exit(result.returncode)\n"
    )
    wrapper.chmod(0o755)
    config_path = mux_server.home_path / ".config/workrex/config.yaml"
    config = yaml.safe_load(config_path.read_text())
    config["remotes"]["lo"]["binary"] = str(wrapper)
    config_path.write_text(yaml.safe_dump(config))
    setup(mux_server, workrex_exe_path, mux_repo_path, node)
    before = node.sessions()
    process, master = start_shell_cli(mux_server, workrex_exe_path, mux_repo_path)
    if response == "delayed":
        assert poll_until(lambda: len(node.sessions()) == 2, timeout=15)
        process.send_signal(signal.SIGTERM)
    finish_shell_cli(process, master)
    assert poll_until(lambda: node.sessions() == before, timeout=10), node.sessions()


def test_shell_close_respects_recorded_server_instance(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    workspace = setup(mux_server, workrex_exe_path, mux_repo_path, node)
    opened = open_shell(node, workrex_exe_path, workspace)
    path = node.root / "state/workspaces" / f"{workspace['ws_id']}.json"
    changed = json.loads(path.read_text())
    changed["execution"]["rex"]["instance_id"] = "previous-server-instance"
    path.write_text(json.dumps(changed))
    closed = close_shell(node, workrex_exe_path, workspace)
    assert closed == {"action": "closed", "closed": False}
    assert opened["session_id"] in {
        session["session_id"] for session in node.rex.api("session.list")["sessions"]
    }
