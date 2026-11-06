"""Native Rex workspace workflows over explicitly configured real SSH hosts."""

import json
import shlex
import subprocess
import uuid

import pytest
import yaml

from ..conftest import get_worktree_path, poll_until
from .conftest import agent_dirs_env, commit_project, git, local_records, wm
from .test_remote_host import BINARY, HOST, remote_sh
from .test_remote_host import pytestmark as ssh_marks

pytestmark = ssh_marks


class SshWorkspace:
    def __init__(self, env, exe, repo, root):
        self.env, self.exe, self.repo, self.root = env, exe, repo, root

    def run(self, command, **kwargs):
        return wm(
            self.env,
            self.exe,
            self.repo,
            command,
            extra_env={"WORKREX_REMOTE_LOOPBACK": "0"},
            timeout=180,
            **kwargs,
        )

    def show(self, handle):
        result = subprocess.run(
            [str(self.exe), "open", handle],
            cwd=self.repo,
            env={
                **self.env.env,
                **agent_dirs_env(self.env.home_path),
                "WORKREX_REMOTE_LOOPBACK": "0",
                "REX_SERVER": self.env.server_url,
                "REX_SESSION": self.env.session_id,
                "REX_BLOCK": self.env.runner_block_id,
            },
            capture_output=True,
            text=True,
            timeout=180,
        )
        assert result.returncode == 0 or "Rex.app" in result.stderr, (
            result.stdout + result.stderr
        )
        return result

    def shell(self, command):
        result = remote_sh(command)
        assert result.returncode == 0, result.stdout + result.stderr
        return result.stdout.strip()

    def record(self, handle):
        return next(
            record for record in local_records(self.env) if record["handle"] == handle
        )

    def at(self, handle, command):
        return self.shell(
            f"cd {shlex.quote(self.record(handle)['remote_worktree'])} && {command}"
        )

    def rex_api(self, method, args):
        command = shlex.join(
            ["rex", "--autostart=false", "api", "call", method, json.dumps(args)]
        )
        return json.loads(self.shell(command))

    def block_call(self, block, method, args=None):
        command = shlex.join(
            [
                "rex",
                "--autostart=false",
                "block",
                "call",
                "-b",
                block,
                method,
                json.dumps(args or {}),
            ]
        )
        return json.loads(self.shell(command))

    def capture(self, handle, pane=0):
        block = self.record(handle)["execution"]["rex"]["blocks"][str(pane)]
        return self.block_call(block, "format", {"format": "text", "trim": True}).get(
            "content", ""
        )

    def send(self, handle, text, pane=0):
        block = self.record(handle)["execution"]["rex"]["blocks"][str(pane)]
        self.block_call(block, "write", {"data": text + "\r"})

    def wait_ready(self, handle, text, pane=0):
        assert poll_until(lambda: text in self.capture(handle, pane), timeout=30), (
            self.capture(handle, pane)
        )

    def node_shell(self, handle, action, slot, host=False):
        record = self.record(handle)
        payload = {
            "action": action,
            "ws_id": record["ws_id"],
            "ex_id": record["execution"]["ex_id"],
            "slot": slot,
        }
        if action == "open":
            payload["host"] = host
        command = shlex.join(
            [
                "env",
                f"WORKREX_NODE_ROOT={self.root}",
                f"{self.root}/bin/{record['execution']['node_bin']}",
                "node",
                "shell",
                "--json",
            ]
        )
        return json.loads(
            self.shell(f"printf %s {shlex.quote(json.dumps(payload))} | {command}")
        )


@pytest.fixture
def ssh_workspace(mux_server, workrex_exe_path, mux_repo_path):
    assert HOST and BINARY
    result = remote_sh('printf %s "$HOME"')
    assert result.returncode == 0, result.stderr
    root = f"{result.stdout}/.workrex-ssh-e2e-{uuid.uuid4().hex[:8]}"
    config_dir = mux_server.home_path / ".config/workrex"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.yaml").write_text(
        yaml.safe_dump(
            {
                "nerdfont": False,
                "remotes": {
                    "lo": {
                        "host": HOST,
                        "path": root,
                        "binary": BINARY,
                        "isolation": "none",
                        "terminal": "rex",
                    }
                },
            }
        )
    )
    workspace = SshWorkspace(mux_server, workrex_exe_path, mux_repo_path, root)
    try:
        yield workspace
    finally:
        for record in list(local_records(mux_server)):
            workspace.run(f"remove --force {shlex.quote(record['handle'])}")
        # The Rex server may also host real user sessions. Only node-owned
        # sessions are removed through the recorded workspace cleanup above.
        remote_sh(f"rm -rf -- {shlex.quote(root)}")


def test_ssh_lifecycle_keeps_one_native_session_and_brings_work_home(ssh_workspace):
    ws = ssh_workspace
    commit_project(
        ws.repo,
        {
            "panes": [
                {"command": "echo SSH-READY"},
                {"command": "echo SECOND-READY", "split": "horizontal"},
            ]
        },
    )
    ws.run("add feat --remote lo")
    ws.wait_ready("feat", "SSH-READY")
    ws.wait_ready("feat", "SECOND-READY", 1)
    record = ws.record("feat")
    placement = record["execution"]["rex"]
    assert len(set(placement["sessions"].values())) == 1
    assert len(set(placement["blocks"].values())) == 2
    ws.send(
        "feat",
        "printf 'remote work\\n' > work.txt; git add work.txt; git -c user.name=Agent -c user.email=a@x commit -m work; echo LIVE-$((6*7))",
    )
    ws.wait_ready("feat", "LIVE-42")
    ws.show("feat")
    assert ws.record("feat")["execution"]["rex"]["blocks"] == placement["blocks"]
    merged = ws.run("merge feat")
    assert "Successfully merged 'feat'" in merged.stdout
    assert git(ws.repo, "show", "main:work.txt") == "remote work"
    assert local_records(ws.env) == []


def test_ssh_rebase_and_pull_preserve_work(ssh_workspace):
    ws = ssh_workspace
    (ws.repo / "shared.txt").write_text("base\n")
    git(ws.repo, "add", "shared.txt")
    git(ws.repo, "commit", "-m", "base")
    commit_project(ws.repo, {"panes": [{"command": "echo READY"}]})
    ws.run("add feat --remote lo")
    ws.wait_ready("feat", "READY")
    ws.at(
        "feat",
        "printf 'remote\\n' > new.txt; git add new.txt; git -c user.name=Agent -c user.email=a@x commit -m remote",
    )
    (ws.repo / "local.txt").write_text("local\n")
    git(ws.repo, "add", "local.txt")
    git(ws.repo, "commit", "-m", "local")
    ws.run("rebase feat")
    assert ws.at("feat", "cat local.txt") == "local"
    ws.run("remote pull feat")
    copy = get_worktree_path(ws.repo, "feat")
    assert (copy / "new.txt").read_text() == "remote\n"
    assert (copy / "local.txt").read_text() == "local\n"
    assert local_records(ws.env) == []


def test_ssh_recorded_container_isolation_survives_config_change(ssh_workspace):
    ws = ssh_workspace
    if remote_sh("docker info >/dev/null 2>&1").returncode:
        pytest.skip("configured SSH host has no Docker daemon")
    config_path = ws.env.home_path / ".config/workrex/config.yaml"
    config = yaml.safe_load(config_path.read_text())
    config["remotes"]["lo"]["isolation"] = "container"
    config_path.write_text(yaml.safe_dump(config))
    commit_project(
        ws.repo, {"panes": [{"command": "test -f /.dockerenv && echo CONTAINER-READY"}]}
    )
    ws.run("add box --remote lo")
    ws.wait_ready("box", "CONTAINER-READY")
    config["remotes"]["lo"]["isolation"] = "none"
    config_path.write_text(yaml.safe_dump(config))
    ws.show("box")
    opened = ws.node_shell("box", "open", "shell1")
    assert opened["container"] is True
    ws.node_shell("box", "close", "shell1")
