"""Remote status, commands, and observation use native Rex block identities."""

import json
import subprocess
from pathlib import Path
from typing import List

import yaml

from ..rex_env import RexEnvironment
from ..conftest import poll_until
from ..support.bin_name import hook_bin
from ..support.codex import CODEX_SIMULATOR
from .conftest import LOOPBACK, Node, git, observation_path, wm


class FakeAgent:
    """A script that applies statuses written to its control file."""

    def __init__(self, directory: Path):
        self.control = directory / "agent-control"
        self.path = directory / "fake-agent"
        self.path.write_text(
            f"""#!/bin/sh
control={self.control}
echo FAKE-AGENT-READY
while :; do
  if [ -f "$control" ]; then
    command=$(cat "$control")
    rm -f "$control"
    case $command in
      exit) exit 0 ;;
      title) printf '\033]2;fake-title-42\007' && echo applied-title ;;
      *) {hook_bin()} set-window-status "$command" </dev/null && echo "applied-$command" ;;
    esac
  fi
  sleep 0.1
done
"""
        )
        self.path.chmod(0o755)

    def do(self, command: str) -> None:
        tmp = self.control.with_suffix(".tmp")
        tmp.write_text(command)
        tmp.rename(self.control)


def setup_agent_project(repo: Path, agent: FakeAgent) -> None:
    project = {
        "nerdfont": False,
        "agent": str(agent.path),
        "panes": [{"command": "<agent>"}],
    }
    (repo / ".workrex.yaml").write_text(yaml.dump(project))
    git(repo, "add", ".workrex.yaml")
    git(repo, "commit", "-m", "configure a fake agent")


def observed_since(env: RexEnvironment, since: float) -> bool:
    """Whether a connected observation arrived after `since`."""
    observed = json.loads(observation_path(env).read_text())
    return observed["connected"] and observed["last_seen"] > since


def status_rows(env: RexEnvironment, exe: Path, repo: Path, *args: str) -> List[dict]:
    result = wm(env, exe, repo, " ".join(["status", "--json", *args]))
    return [row for row in json.loads(result.stdout)["agents"] if row.get("remote")]


def start_workrex(
    env: RexEnvironment, exe: Path, repo: Path, *args: str
) -> "subprocess.Popen[str]":
    """Run a command without changing any Rex app selection."""
    process_env = {**env.env, **LOOPBACK}
    return subprocess.Popen(
        [str(exe), *args],
        cwd=repo,
        env=process_env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def test_send_reaches_a_remote_agent_the_config_picks(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    tmp_path: Path,
):
    """Without `-a`, the configured agent decides how input is typed."""
    repo = mux_repo_path
    exe = workrex_exe_path
    simulator = tmp_path / "codex_simulator.py"
    simulator.write_text(CODEX_SIMULATOR)
    ready = tmp_path / "codex-ready"
    submitted = tmp_path / "codex-submitted"
    received = tmp_path / "codex-input"
    # Under an interpreter, the pane does not look like Codex.
    mux_server.install_script(
        mux_server.fake_bin_dir / "codex",
        f"#!/bin/sh\nexec python3 {simulator} {hook_bin()} {ready} {submitted} {received}\n",
    )
    project = {"nerdfont": False, "agent": "codex", "panes": [{"command": "<agent>"}]}
    (repo / ".workrex.yaml").write_text(yaml.dump(project))
    git(repo, "add", ".workrex.yaml")
    git(repo, "commit", "-m", "configure codex")

    wm(mux_server, exe, repo, "add cx --remote lo")
    assert poll_until(ready.exists, timeout=20), "Codex simulator did not start"
    assert poll_until(
        lambda: [r["status"] for r in status_rows(mux_server, exe, repo)]
        == ["waiting"],
        timeout=10,
    )
    prompt = "continue with deterministic delivery"
    wm(mux_server, exe, repo, f"send cx '{prompt}'")
    assert poll_until(submitted.exists, timeout=10), (
        f"prompt was not pasted; input={received.read_bytes()!r}"
    )
    assert submitted.read_text() == prompt


def test_an_older_observer_does_not_hide_the_node(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    tmp_path: Path,
):
    repo = mux_repo_path
    exe = workrex_exe_path
    agent = FakeAgent(tmp_path)
    setup_agent_project(repo, agent)
    wm(mux_server, exe, repo, "add mix --remote lo")
    node.wait_for_agent(mux_server, "mix", "FAKE-AGENT-READY")

    # An older build observes the remote: it holds its lock and keeps writing
    # a connected observation in its own format that lists no agents.
    directory = observation_path(mux_server).parent
    legacy = directory / "observed.v3.json"
    holder = subprocess.Popen(
        [
            "python3",
            "-c",
            "import fcntl, json, sys, time\n"
            "lock = open(sys.argv[1], 'a')\n"
            "fcntl.flock(lock, fcntl.LOCK_EX)\n"
            "while True:\n"
            "    data = {'connected': True, 'last_seen': int(time.time()),"
            " 'skew': 0, 'workspaces': [], 'retired': 1}\n"
            "    open(sys.argv[2], 'w').write(json.dumps(data))\n"
            "    time.sleep(0.5)\n",
            str(directory / "observer.v3.lock"),
            str(legacy),
        ]
    )
    try:
        assert poll_until(legacy.exists, timeout=5)
        agent.do("working")
        assert poll_until(
            lambda: [r["status"] for r in status_rows(mux_server, exe, repo)]
            == ["working"],
            timeout=15,
        )
        [row] = status_rows(mux_server, exe, repo)
        assert row["remote"]["connected"] is True
    finally:
        holder.kill()
        holder.wait()


def test_remote_status_reaches_native_observation(
    mux_server, workrex_exe_path, mux_repo_path, node, tmp_path
):
    agent = FakeAgent(tmp_path)
    setup_agent_project(mux_repo_path, agent)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add st --remote lo")
    node.wait_for_agent(mux_server, "st", "FAKE-AGENT-READY")
    [workspace] = node.workspaces()
    block = workspace["execution"]["rex"]["blocks"]["0"]
    for status in ["working", "waiting", "done"]:
        agent.do(status)
        node.wait_for_agent(mux_server, "st", "applied-" + status)
        assert poll_until(
            lambda: [
                row["status"]
                for row in status_rows(mux_server, workrex_exe_path, mux_repo_path)
            ]
            == [status],
            timeout=15,
        )
    [row] = status_rows(mux_server, workrex_exe_path, mux_repo_path)
    assert row["worktree"] == "st"
    assert row["remote"]["name"] == "lo" and row["remote"]["connected"]
    assert block.startswith("block:")
    captured = wm(mux_server, workrex_exe_path, mux_repo_path, "capture st")
    assert "applied-done" in captured.stdout


def test_native_send_capture_and_wait_need_no_local_window(
    mux_server, workrex_exe_path, mux_repo_path, node, tmp_path
):
    agent = FakeAgent(tmp_path)
    setup_agent_project(mux_repo_path, agent)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add cmd --remote lo")
    node.wait_for_agent(mux_server, "cmd", "FAKE-AGENT-READY")
    agent.do("working")
    assert poll_until(
        lambda: [
            row["status"]
            for row in status_rows(mux_server, workrex_exe_path, mux_repo_path)
        ]
        == ["working"],
        timeout=15,
    )
    wm(mux_server, workrex_exe_path, mux_repo_path, "send cmd remote-hello-42")
    assert poll_until(
        lambda: "remote-hello-42"
        in wm(mux_server, workrex_exe_path, mux_repo_path, "capture cmd").stdout,
        timeout=10,
    )
    waiting = start_workrex(
        mux_server, workrex_exe_path, mux_repo_path, "wait", "cmd", "--timeout", "30"
    )
    try:
        agent.do("done")
        _, errors = waiting.communicate(timeout=40)
        assert waiting.returncode == 0, errors
    finally:
        if waiting.poll() is None:
            waiting.kill()
            waiting.wait()
    workspace = node.workspace("cmd")
    target = {"ws_id": workspace["ws_id"], "pane": 99, "lines": 10}
    missing = node.result(workrex_exe_path, "capture", target)
    assert missing.returncode != 0
    assert json.loads(missing.stdout)["error"]["code"] == "not_found"
