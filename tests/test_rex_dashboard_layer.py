"""
Tests for resources/rex/workrex.lua, the Rex action that opens the dashboard
in a floating layer. Each test runs its own Rex server with the file as its
configuration.
"""

import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Callable, Generator

import pytest

from .conftest import rex_binary

LUA = Path(__file__).parent.parent / "resources/rex/workrex.lua"
SHELL = "com.superlogical.terminal.shell"

pytestmark = pytest.mark.skipif(rex_binary() is None, reason="rex CLI not found")


class RexServer:
    def __init__(self, workmux: Path, tmp_path: Path):
        self.rex = rex_binary()
        # macOS limits socket paths to 104 bytes; pytest tmp paths run longer.
        self.dir = Path(tempfile.mkdtemp(prefix="rx", dir="/tmp"))
        self.url = f"unix://{self.dir / 's.sock'}"
        self.env = {
            key: value
            for key, value in os.environ.items()
            if key not in ("REX_BLOCK", "REX_SESSION", "REX_CLIENT", "TMUX")
        }
        self.env.update(
            REX_SERVER=self.url,
            WORKREX_BIN=str(workmux),
            XDG_STATE_HOME=str(tmp_path / "state"),
            XDG_CONFIG_HOME=str(tmp_path / "config"),
        )
        log = open(self.dir / "server.log", "w")
        self.process = subprocess.Popen(
            [self.rex, "server", "run", "--listen", self.url]
            + ["--state-dir", str(self.dir / "state"), "--restore=false"]
            + ["--config", str(LUA)],
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        log.close()
        wait_until(lambda: self.cli("whoami", check=False).returncode == 0, 15)

    def stop(self) -> None:
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        shutil.rmtree(self.dir, ignore_errors=True)

    def cli(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [self.rex, "--autostart=false", "-S", self.url, *args],
            env=self.env,
            capture_output=True,
            text=True,
            check=check,
        )

    def api(self, method: str, args: dict) -> dict:
        return json.loads(self.cli("api", "call", method, json.dumps(args)).stdout)

    def new_session(self) -> tuple[str, str]:
        layout = {"block": {"flavor": SHELL}}
        created = self.api(
            "session.create",
            {"label": "test", "initial_windows": [{"layout": layout}]},
        )
        return created["session_id"], created["initial_windows"][0]["block_ids"][0]

    def open_dashboard(self, session: str) -> dict:
        return json.loads(self.cli("do", "-s", session, "workrex_dashboard").stdout)

    def window(self, session: str) -> dict:
        return self.api("session.view", {"session_id": session})["windows"][0]

    def screen(self, block: str) -> str:
        return self.cli("capture", "-b", block, "--trim", check=False).stdout


def wait_until(predicate: Callable[[], bool], timeout: float = 10) -> None:
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() > deadline:
            raise AssertionError("timed out")
        time.sleep(0.1)


def floating_layers(window: dict) -> list[dict]:
    return [layer for layer in window["layers"] if layer["kind"] == "floating"]


@pytest.fixture
def rex(workmux_exe_path: Path, tmp_path: Path) -> Generator[RexServer, None, None]:
    server = RexServer(workmux_exe_path, tmp_path)
    try:
        yield server
    finally:
        server.stop()


def test_lua_file_passes_config_check():
    result = subprocess.run(
        [rex_binary(), "config", "check", str(LUA)], capture_output=True, text=True
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "actions: 1" in result.stdout


def test_server_lists_the_dashboard_action(rex: RexServer):
    assert "workrex_dashboard" in rex.cli("actions").stdout


def test_action_opens_one_dashboard_layer_and_q_closes_it(rex: RexServer):
    session, shell = rex.new_session()

    opened = rex.open_dashboard(session)
    window = rex.window(session)
    [layer] = floating_layers(window)
    assert layer["layer_id"] == opened["layer_id"]
    assert [b["label"] for b in layer["blocks"]] == ["workrex-dashboard"]
    assert window["focused_block_id"] == opened["block_id"]
    wait_until(lambda: "Agents" in rex.screen(opened["block_id"]))

    again = rex.open_dashboard(session)
    assert again["block_id"] == opened["block_id"]
    assert len(floating_layers(rex.window(session))) == 1

    rex.cli("send", "-b", opened["block_id"], "q")
    wait_until(lambda: not floating_layers(rex.window(session)))
    assert rex.window(session)["focused_block_id"] == shell
