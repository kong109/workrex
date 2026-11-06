"""Fixtures for `workrex remote` and `workrex node` tests."""

import fcntl
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Any, Dict, Iterator, List, Optional

import pytest
import yaml

from ..conftest import (
    MuxEnvironment,
    poll_until,
    run_workrex_command,
)
from ..rex_env import RexEnvironment
from ..support.bin_name import BIN


def agent_dirs_env(home: Path) -> Dict[str, str]:
    """Agent directory overrides inside `home`, so a loopback node installing
    status hooks never writes to the real agent config the environment names."""
    return {
        "CLAUDE_CONFIG_DIR": str(home / ".claude"),
        "CODEX_HOME": str(home / ".codex"),
        "COPILOT_HOME": str(home / ".copilot"),
        "COPILOT_CONFIG_DIR": str(home / ".copilot"),
        "GROK_HOME": str(home / ".grok"),
        "PI_CONFIG_DIR": str(home / ".omp"),
        "OPENCODE_CONFIG": str(home / ".config" / "opencode" / "opencode.json"),
        "GEMINI_CLI_HOME": str(home),
        "PI_CODING_AGENT_DIR": str(home / ".pi" / "agent"),
        "OPENCODE_CONFIG_DIR": str(home / ".config" / "opencode"),
    }


class RemoteCli:
    """Runs workrex with isolated HOME/XDG dirs and a generated global config."""

    def __init__(self, tmp_path: Path, exe: Path, loopback: bool):
        self.exe = exe
        self.home = tmp_path / "home"
        self.cwd = tmp_path / "work"
        self.home.mkdir()
        self.cwd.mkdir()
        self.env = os.environ.copy()
        self.env.update(
            {
                "HOME": str(self.home),
                # Agent discovery must not depend on executables on the host PATH.
                "WORKREX_TEST": "1",
                "WORKREX_BACKEND": "rex",
                "REX_SERVER": f"unix://{self.cwd}/unavailable.sock",
                "XDG_CONFIG_HOME": str(self.home / ".config"),
                "XDG_STATE_HOME": str(self.home / ".local" / "state"),
                "XDG_CACHE_HOME": str(self.home / ".cache"),
                **agent_dirs_env(self.home),
            }
        )
        # Containers mount pi's and omp's default directories, which
        # PI_CODING_AGENT_DIR would make one.
        for name in ("PI_CODING_AGENT_DIR", "PI_CONFIG_DIR"):
            self.env.pop(name, None)
        self.env.pop("WORKREX_TEST_AGY_DETECT", None)
        self.env.pop("REX_SESSION", None)
        self.env.pop("REX_BLOCK", None)
        if loopback:
            self.env["WORKREX_REMOTE_LOOPBACK"] = "1"
        else:
            self.env.pop("WORKREX_REMOTE_LOOPBACK", None)

    def write_config(
        self,
        remotes: Dict[str, Dict[str, Any]],
        placement: Optional[str] = None,
        **extra: Any,
    ) -> None:
        config: Dict[str, Any] = {"nerdfont": False, "remotes": remotes, **extra}
        if placement is not None:
            config["placement"] = placement
        config_dir = self.home / ".config" / "workrex"
        config_dir.mkdir(parents=True, exist_ok=True)
        (config_dir / "config.yaml").write_text(yaml.dump(config))

    def run(
        self, *args: str, input: str = "", timeout: float = 120
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.exe), *args],
            cwd=self.cwd,
            env=self.env,
            input=input,
            capture_output=True,
            text=True,
            timeout=timeout,
        )


def dev_pin(binary: Path) -> str:
    """Pinned file name `workrex remote setup` uses for an uploaded binary."""
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    return f"{BIN}-dev-{digest[:12]}"


def write_script(path: Path, body: str) -> Path:
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)
    return path


@pytest.fixture
def loopback_cli(tmp_path: Path, workrex_exe_path: Path) -> RemoteCli:
    return RemoteCli(tmp_path, workrex_exe_path, loopback=True)


LOOPBACK = {"WORKREX_REMOTE_LOOPBACK": "1"}


class Node:
    """The loopback node and its recorded sessions on an isolated Rex server."""

    def __init__(self, root: Path, rex: RexEnvironment):
        self.root = root
        self.rex = rex
        self.tag = "wr" + hashlib.sha256(str(root).encode()).hexdigest()[:8] + "."

    def workspaces(self) -> List[dict]:
        directory = self.root / "state" / "workspaces"
        if not directory.is_dir():
            return []
        return [
            json.loads(path.read_text())
            for path in sorted(directory.glob("w*.json"))
            if "." not in path.stem
        ]

    def workspace(self, handle: str) -> dict:
        return next(
            workspace
            for workspace in self.workspaces()
            if workspace["handle"] == handle
        )

    def execution(self, ex_id: str) -> dict:
        return next(
            workspace["execution"]
            for workspace in self.workspaces()
            if (workspace.get("execution") or {}).get("ex_id") == ex_id
        )

    def rex_sessions(self) -> Dict[str, str]:
        """Owned physical sessions, including recorded sessions the user renamed."""
        recorded = {}
        for workspace in self.workspaces():
            execution = workspace.get("execution") or {}
            placement = execution.get("rex") or {}
            for index, session in sorted(placement.get("sessions", {}).items()):
                recorded.setdefault(session, f"{execution['ex_id']}-{index}")
            for slot, session in placement.get("shells", {}).items():
                recorded[session] = f"{execution['ex_id']}-{slot}"
        sessions = {}
        for session in self.rex.api("session.list")["sessions"]:
            identity = session["session_id"]
            if identity not in recorded and not session["label"].startswith(self.tag):
                continue
            name = recorded.get(identity, session["label"][len(self.tag) :])
            if name in sessions:
                name += "#" + identity
            sessions[name] = identity
        return sessions

    def sessions(self) -> List[str]:
        return sorted(self.rex_sessions())

    def plant_session(self, name: str) -> str:
        created = self.rex.api(
            "session.create",
            {
                "label": self.tag + name,
                "initial_windows": [{"layout": self.rex._shell_layout()}],
            },
        )
        return created["session_id"]

    def session_id(self, target: str) -> str:
        target = target.lstrip("=").rstrip(":")
        if target.startswith("session:"):
            return target
        if target in self.rex_sessions():
            return self.rex_sessions()[target]
        ex_id, _, slot = target.rpartition("-")
        execution = self.execution(ex_id)
        placement = execution["rex"]
        return (
            placement["shells"][slot]
            if slot.startswith("shell")
            else placement["sessions"][slot]
        )

    def view(self, target: str) -> dict:
        return self.rex.api("session.view", {"session_id": self.session_id(target)})

    def blocks(self, target: str) -> List[str]:
        return [
            block["block_id"]
            for window in self.view(target)["windows"]
            for layer in window["layers"]
            for block in layer["blocks"]
            if block.get("flavor") == self.rex.SHELL_FLAVOR
        ]

    def pane(self, ex_id: str, index: int = 0) -> str:
        return self.execution(ex_id)["rex"]["blocks"][str(index)]

    def block_for(self, target: str) -> str:
        if target.startswith("block:"):
            return target
        raw = target.lstrip("=").rstrip(":")
        ex_id, _, slot = raw.rpartition("-")
        if slot.isdigit():
            try:
                return self.pane(ex_id, int(slot))
            except (KeyError, StopIteration):
                pass
        return self.blocks(raw)[0]

    def capture(self, target: str) -> str:
        try:
            block = self.block_for(target)
        except (KeyError, StopIteration):
            return ""
        result = self.rex.mux_command(
            ["capture", "-b", block, "--trim", "--unwrap"], check=False
        )
        return result.stdout if result.returncode == 0 else ""

    def send(self, target: str, text: str, enter: bool = True) -> None:
        self.rex.send_keys(self.block_for(target), text, enter=enter)

    def kill_session(self, target: str) -> None:
        self.rex.api("session.destroy", {"session_id": self.session_id(target)})

    def stop_sessions(self) -> None:
        for session in set(self.rex_sessions().values()):
            self.rex.api("session.destroy", {"session_id": session})

    def wait_for_agent(
        self, env: MuxEnvironment, branch: str, text: str, timeout: float = 20
    ) -> None:
        def captures():
            try:
                execution = self.workspace(branch)["execution"]
                return [
                    self.capture(block)
                    for block in execution["rex"].get("blocks", {}).values()
                ]
            except (KeyError, StopIteration):
                return []

        assert poll_until(
            lambda: any(text in output for output in captures()), timeout=timeout
        ), captures()

    def result(
        self, exe: Path, verb: str, payload: dict
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(exe), "node", verb, "--json"],
            input=json.dumps(payload),
            env={**self.rex.env, **LOOPBACK, "WORKREX_NODE_ROOT": str(self.root)},
            capture_output=True,
            text=True,
            timeout=30,
        )

    def call(self, exe: Path, verb: str, payload: dict) -> dict:
        result = self.result(exe, verb, payload)
        assert result.returncode == 0, result.stdout + result.stderr
        return json.loads(result.stdout)


@pytest.fixture
def node(mux_server: MuxEnvironment, workrex_exe_path: Path) -> Iterator[Node]:
    assert isinstance(mux_server, RexEnvironment)
    root = Path(tempfile.mkdtemp(prefix="wrn-", dir="/tmp")).resolve()
    remote = {
        "host": "localhost",
        "path": str(root),
        "binary": str(workrex_exe_path),
        "isolation": "none",
        "terminal": "rex",
    }
    config = {"nerdfont": False, "remotes": {"lo": remote}}
    config_dir = mux_server.home_path / ".config" / "workrex"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.yaml").write_text(yaml.dump(config))
    hosted = Node(root, mux_server)
    try:
        yield hosted
    finally:
        records = mux_server.home_path / ".local/state/workrex/remote/lo/workspaces"
        shutil.rmtree(records, ignore_errors=True)
        try:
            assert poll_until(
                lambda: not observer_owner_running(mux_server), timeout=10
            )
        finally:
            hosted.stop_sessions()
            shutil.rmtree(root, ignore_errors=True)


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


def commit_project(repo: Path, project: Dict[str, Any]) -> None:
    """Commit `project` as the repository's `.workrex.yaml`."""
    (repo / ".workrex.yaml").write_text(yaml.dump(project))
    git(repo, "add", ".workrex.yaml")
    git(repo, "commit", "-m", "configure the project")


def local_records(env: RexEnvironment) -> List[dict]:
    directory = env.home_path / ".local/state/workrex/remote/lo/workspaces"
    if not directory.is_dir():
        return []
    return [json.loads(p.read_text()) for p in sorted(directory.glob("*.json"))]


def observer_owner_running(env: RexEnvironment) -> bool:
    lease = env.home_path / ".local/state/workrex/remote/lo/observer-owner.v4.lock"
    if not lease.exists():
        return False
    with lease.open("r+") as file:
        try:
            fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(file, fcntl.LOCK_UN)
        return False


def observation_path(env: RexEnvironment) -> Path:
    """The laptop's observation of the loopback remote."""
    return env.home_path / ".local/state/workrex/remote/lo/observed.v4.json"


def wm(
    env: MuxEnvironment,
    exe: Path,
    repo: Path,
    command: str,
    extra_env: Optional[Dict[str, str]] = None,
    **kwargs,
):
    """Run workrex from the runner pane; commands that focus a remote window
    always start from the fixture's known session and block."""
    env.select_window("test")
    kwargs.setdefault("timeout", 60)
    pre_run_env = {**LOOPBACK, **agent_dirs_env(env.home_path), **(extra_env or {})}
    return run_workrex_command(
        env, exe, repo, command, pre_run_env=pre_run_env, **kwargs
    )
