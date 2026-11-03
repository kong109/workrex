"""Fixtures for `workmux remote` and `workmux node` tests."""

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
    TmuxEnvironment,
    get_window_name,
    poll_until,
    run_workmux_command,
    wait_for_pane_output,
)
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
    """Runs workmux with isolated HOME/XDG dirs and a generated global config."""

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
                "WORKMUX_TEST": "1",
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
        self.env.pop("WORKMUX_TEST_AGY_DETECT", None)
        self.env.pop("TMUX", None)
        self.env.pop("TMUX_PANE", None)
        if loopback:
            self.env["WORKMUX_REMOTE_LOOPBACK"] = "1"
        else:
            self.env.pop("WORKMUX_REMOTE_LOOPBACK", None)

    def write_config(
        self,
        remotes: Dict[str, Dict[str, Any]],
        placement: Optional[str] = None,
        **extra: Any,
    ) -> None:
        config: Dict[str, Any] = {"nerdfont": False, "remotes": remotes, **extra}
        if placement is not None:
            config["placement"] = placement
        config_dir = self.home / ".config" / "workmux"
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
def loopback_cli(tmp_path: Path, workmux_exe_path: Path) -> RemoteCli:
    return RemoteCli(tmp_path, workmux_exe_path, loopback=True)


LOOPBACK = {"WORKMUX_REMOTE_LOOPBACK": "1"}

# Where the loopback node hosts agent terminals: its own tmux server, or the
# test's isolated Rex server standing in for the node's.
NODE_TERMINAL = os.environ.get("WORKREX_TEST_NODE_TERMINAL", "tmux")


class Node:
    """The loopback node: its root, records, and the tmux or Rex server its
    agent terminals run on."""

    def __init__(self, root: Path, rex: Optional[Any] = None):
        self.root = root
        self.rex = rex
        # Rex sessions of this root carry this prefix (`root_tag` in host.rs).
        self.tag = "wr" + hashlib.sha256(str(root).encode()).hexdigest()[:8] + "."

    def rex_sessions(self) -> Dict[str, str]:
        """This root's Rex sessions, by name without the tag, to their ids."""
        if self.rex is None:
            return {}
        return {
            s["label"][len(self.tag) :]: s["session_id"]
            for s in self.rex.api("session.list")["sessions"]
            if s["label"].startswith(self.tag)
        }

    def plant_session(self, name: str) -> None:
        """Start a bare session `name` where this node hosts its terminals."""
        if self.rex is None:
            self.tmux("new-session", "-d", "-s", name)
        else:
            self.rex.api(
                "session.create",
                {
                    "label": self.tag + name,
                    "initial_windows": [{"layout": self.rex._shell_layout()}],
                },
            )

    def capture(self, name: str) -> str:
        """The visible text of session `name`'s agent pane."""
        if self.rex is None:
            return self.tmux("capture-pane", "-p", "-t", f"={name}:").stdout
        session = self.rex_sessions().get(name)
        if session is None:
            return ""
        view = self.rex.api("session.view", {"session_id": session})
        blocks = [
            block["block_id"]
            for window in view["windows"]
            for layer in window["layers"]
            for block in layer["blocks"]
        ]
        reports = [
            self.rex.block_call(b, "format", {"format": "text", "trim": True})
            for b in blocks
        ]
        return "\n".join(r.get("content", "") for r in reports if r)

    def tmux(self, *args: str) -> subprocess.CompletedProcess[str]:
        socket = self.root / "run" / "tmux.sock"
        return subprocess.run(
            ["tmux", "-S", str(socket), *args], capture_output=True, text=True
        )

    def sessions(self) -> List[str]:
        if self.rex is not None:
            return sorted(self.rex_sessions())
        result = self.tmux("list-sessions", "-F", "#{session_name}")
        return sorted(result.stdout.split()) if result.returncode == 0 else []

    def wait_for_agent(
        self, env: MuxEnvironment, branch: str, text: str, timeout: float = 20
    ) -> None:
        """Wait until the agent of `branch` prints `text`: in its local relay
        window, or on the node's Rex server for a Rex-hosted terminal."""
        if self.rex is None:
            wait_for_pane_output(env, get_window_name(branch), text, timeout=timeout)
            return
        assert poll_until(
            lambda: any(text in self.capture(name) for name in self.sessions()),
            timeout=timeout,
        ), f"{text!r} not on the node: {[self.capture(n) for n in self.sessions()]}"

    def workspaces(self) -> List[dict]:
        directory = self.root / "state" / "workspaces"
        if not directory.is_dir():
            return []
        return [
            json.loads(path.read_text())
            for path in sorted(directory.glob("w*.json"))
            if "." not in path.stem
        ]


@pytest.fixture
def node(mux_server: MuxEnvironment, workmux_exe_path: Path) -> Iterator[Node]:
    # A short root keeps the node's tmux socket under the root.
    root = Path(tempfile.mkdtemp(prefix="wmn-", dir="/tmp")).resolve()
    remote = {
        "host": "localhost",
        "path": str(root),
        "binary": str(workmux_exe_path),
        "isolation": "none",
    }
    rex = None
    if NODE_TERMINAL == "rex":
        if mux_server.backend_name != "rex":
            pytest.skip("a Rex-hosted node needs the Rex backend's server")
        remote["terminal"] = "rex"
        rex = mux_server
    config = {"nerdfont": False, "remotes": {"lo": remote}}
    config_dir = mux_server.home_path / ".config" / "workmux"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.yaml").write_text(yaml.dump(config))
    node = Node(root, rex)
    yield node
    for session in node.rex_sessions().values():
        rex.api("session.destroy", {"session_id": session})
    node.tmux("kill-server")
    shutil.rmtree(root, ignore_errors=True)


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


def commit_project(repo: Path, project: Dict[str, Any]) -> None:
    """Commit `project` as the repository's `.workmux.yaml`."""
    (repo / ".workmux.yaml").write_text(yaml.dump(project))
    git(repo, "add", ".workmux.yaml")
    git(repo, "commit", "-m", "configure the project")


def local_records(env: TmuxEnvironment) -> List[dict]:
    directory = env.home_path / ".local/state/workmux/remote/lo/workspaces"
    if not directory.is_dir():
        return []
    return [json.loads(p.read_text()) for p in sorted(directory.glob("*.json"))]


def observation_path(env: TmuxEnvironment) -> Path:
    """The laptop's observation of the loopback remote."""
    return env.home_path / ".local/state/workmux/remote/lo/observed.v3.json"


def wm(
    env: MuxEnvironment,
    exe: Path,
    repo: Path,
    command: str,
    extra_env: Optional[Dict[str, str]] = None,
    **kwargs,
):
    """Run workmux from the runner pane; commands that focus a remote window
    would otherwise type the next command into the remote shell."""
    if isinstance(env, TmuxEnvironment):
        assert env.runner_pane_id is not None
        env.tmux(["select-window", "-t", env.runner_pane_id])
    else:
        env.select_window("test")
    kwargs.setdefault("timeout", 60)
    pre_run_env = {**LOOPBACK, **agent_dirs_env(env.home_path), **(extra_env or {})}
    return run_workmux_command(
        env, exe, repo, command, pre_run_env=pre_run_env, **kwargs
    )
