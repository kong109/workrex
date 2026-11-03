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

from .rex_env import rex_binary

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

    def split_session(
        self, agent: list[str] | None = None, agent_in_new_tab: bool = False
    ) -> tuple[str, str, str]:
        """A session whose first tab holds a shell beside a second block, which
        runs `agent` when given. With `agent_in_new_tab` the agent gets a
        second tab instead. Returns the session, the shell and the agent."""
        shell = {"block": {"flavor": SHELL}}
        agent_block = {"block": {"flavor": SHELL, "options": {"command": agent}}}
        if agent is None:
            agent_block = shell
        if agent_in_new_tab:
            layouts = [{"split": {"direction": "horizontal", "before": shell, "after": shell}}]
            layouts.append(agent_block)
        else:
            split = {"direction": "horizontal", "before": shell, "after": agent_block}
            layouts = [{"split": split}]
        created = self.api(
            "session.create",
            {
                "label": "test",
                "initial_windows": [{"layout": layout} for layout in layouts],
            },
        )
        blocks = [b for w in created["initial_windows"] for b in w["block_ids"]]
        return created["session_id"], blocks[0], blocks[-1]

    def focus(self, session: str, block: str) -> None:
        self.api("session.focus_block", {"session_id": session, "block_id": block})

    def focused(self, session: str) -> str:
        return self.window(session)["focused_block_id"]

    def open_dashboard(self, session: str) -> dict:
        return json.loads(self.cli("do", "-s", session, "workrex_dashboard").stdout)

    def window(self, session: str) -> dict:
        return self.api("session.view", {"session_id": session})["windows"][0]

    def last_exit(self, block: str) -> object:
        process = self.cli("block", "call", "-b", block, "process").stdout
        return json.loads(process).get("last_exit")

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


def test_a_dashboard_that_exited_is_replaced_instead_of_refocused(tmp_path: Path):
    rex = RexServer(Path("/usr/bin/false"), tmp_path)
    try:
        session, _ = rex.new_session()
        dead = rex.open_dashboard(session)
        wait_until(lambda: rex.last_exit(dead["block_id"]) is not None)

        fresh = rex.open_dashboard(session)
        assert fresh["block_id"] != dead["block_id"]
        [layer] = floating_layers(rex.window(session))
        assert [b["block_id"] for b in layer["blocks"]] == [fresh["block_id"]]
    finally:
        rex.stop()


def test_q_returns_focus_to_the_block_the_dashboard_opened_over(rex: RexServer):
    # Rex hands focus to the tab's last block when a layer's program exits.
    session, shell, _ = rex.split_session()
    rex.focus(session, shell)

    opened = rex.open_dashboard(session)
    wait_until(lambda: "Agents" in rex.screen(opened["block_id"]))
    rex.cli("send", "-b", opened["block_id"], "q")
    wait_until(lambda: not floating_layers(rex.window(session)))

    assert rex.focused(session) == shell


HINT = "Turn on Remote Control in Rex Settings to jump across tabs"
NO_ANSWER_HINT = "Rex.app did not respond; jump not made"
PEEK_HINT = "Peek works only for agents in this tab"
REFUSE = "echo 'Error: Remote Control is off for this client (code=refused)' >&2; exit 1"


def refusing_rex(workmux: Path, tmp_path: Path, select: str = REFUSE) -> Path:
    """A workmux whose rex reports an app that owns every block and is
    attached to the sessions `attach_app` names, and answers session.select
    with `select`: by default it fails, as Rex.app does with Remote Control
    off. Each select is logged to `selects.log`. Rex sets REX_BIN_DIR in
    every block, so the wrapper overrides it."""
    bin_dir = tmp_path / "refusing-rex"
    bin_dir.mkdir()
    owner = '{"columns":80,"rows":24,"owner":"client:app"}'
    (bin_dir / "rex").write_text(
        f"""#!/bin/sh
case "$*" in
  *session.select*) echo "$*" >> '{bin_dir / "selects.log"}'; {select} ;;
  *client.list*) cat '{bin_dir / "clients.json"}'; exit 0 ;;
  *"block call -b "*" size "*) echo '{owner}'; exit 0 ;;
esac
exec '{rex_binary()}' "$@"
"""
    )
    (bin_dir / "workmux").write_text(
        f"#!/bin/sh\nREX_BIN_DIR='{bin_dir}' exec '{workmux}' \"$@\"\n"
    )
    for program in ("rex", "workmux"):
        (bin_dir / program).chmod(0o755)
    return bin_dir / "workmux"


def attach_app(rex: RexServer, session: str) -> None:
    clients = {
        "clients": [
            {
                "client_id": "client:app",
                "info": {"kind": "app"},
                "session_ids": [session],
            }
        ]
    }
    bin_dir = Path(rex.env["WORKREX_BIN"]).parent
    (bin_dir / "clients.json").write_text(json.dumps(clients))


@pytest.fixture
def refusing(
    workmux_exe_path: Path, tmp_path: Path
) -> Generator[RexServer, None, None]:
    server = RexServer(refusing_rex(workmux_exe_path, tmp_path), tmp_path)
    try:
        yield server
    finally:
        server.stop()


@pytest.fixture
def silent(workmux_exe_path: Path, tmp_path: Path) -> Generator[RexServer, None, None]:
    """An app that never answers session.select."""
    workmux = refusing_rex(workmux_exe_path, tmp_path, select="exec sleep 30")
    server = RexServer(workmux, tmp_path)
    try:
        yield server
    finally:
        server.stop()


def selects(rex: RexServer) -> list[str]:
    log = Path(rex.env["WORKREX_BIN"]).parent / "selects.log"
    return log.read_text().splitlines() if log.exists() else []


def holds(predicate: Callable[[], bool], seconds: float) -> bool:
    """Whether `predicate` stays true for `seconds`. The dashboard clears
    expiring messages on its 2 s refresh, 1.5 s or more after they appear,
    so 5 s spans two refreshes past any expiry."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if not predicate():
            return False
        time.sleep(0.1)
    return True


def agent_command(rex: RexServer) -> list[str]:
    """A program that reports itself as a working agent and keeps running,
    which keeps it listed."""
    repo = Path(rex.env["XDG_STATE_HOME"]).parent / "repo"
    repo.mkdir(exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    script = 'cd "$1" && "$WORKREX_BIN" set-window-status working && read x'
    return ["sh", "-c", script, "sh", str(repo)]


def open_dashboard_over(rex: RexServer, session: str, shell: str) -> str:
    """Opens the dashboard over `shell` and returns its block once it lists
    the agent."""
    rex.focus(session, shell)
    dashboard = rex.open_dashboard(session)["block_id"]
    wait_until(lambda: "repo" in rex.screen(dashboard), 20)
    return dashboard


def test_refused_jump_within_the_tab_still_lands_and_closes(refusing: RexServer):
    session, shell, agent = refusing.split_session(agent_command(refusing))
    attach_app(refusing, session)
    dashboard = open_dashboard_over(refusing, session, shell)

    refusing.cli("send-key", "-b", dashboard, "Enter")
    wait_until(lambda: not floating_layers(refusing.window(session)))

    assert refusing.focused(session) == agent


def test_refused_jump_to_another_tab_keeps_the_dashboard_and_its_hint(
    refusing: RexServer,
):
    session, shell, _ = refusing.split_session(
        agent_command(refusing), agent_in_new_tab=True
    )
    attach_app(refusing, session)
    dashboard = open_dashboard_over(refusing, session, shell)

    refusing.cli("send-key", "-b", dashboard, "Enter")
    wait_until(lambda: HINT in refusing.screen(dashboard))

    assert holds(lambda: HINT in refusing.screen(dashboard), 5)
    assert refusing.focused(session) == dashboard
    assert len(floating_layers(refusing.window(session))) == 1

    refusing.cli("send", "-b", dashboard, "j")
    wait_until(lambda: HINT not in refusing.screen(dashboard))
    assert refusing.focused(session) == dashboard


def test_peek_keeps_the_keyboard_on_the_dashboard(refusing: RexServer):
    session, shell, agent = refusing.split_session(agent_command(refusing))
    attach_app(refusing, session)
    dashboard = open_dashboard_over(refusing, session, shell)
    agent_screen = refusing.screen(agent)

    refusing.cli("send", "-b", dashboard, "p")
    time.sleep(1)
    assert refusing.focused(session) == dashboard
    refusing.cli("send", "-b", dashboard, "j")
    time.sleep(1)

    assert refusing.focused(session) == dashboard
    assert len(floating_layers(refusing.window(session))) == 1
    assert refusing.screen(agent) == agent_screen
    assert selects(refusing) == []


def test_peek_at_another_tab_keeps_the_dashboard_with_a_hint(refusing: RexServer):
    session, shell, _ = refusing.split_session(
        agent_command(refusing), agent_in_new_tab=True
    )
    attach_app(refusing, session)
    dashboard = open_dashboard_over(refusing, session, shell)

    refusing.cli("send", "-b", dashboard, "p")
    wait_until(lambda: PEEK_HINT in refusing.screen(dashboard))

    assert refusing.focused(session) == dashboard
    assert selects(refusing) == []


def test_unanswered_jump_to_another_tab_keeps_the_dashboard_and_its_hint(
    silent: RexServer,
):
    session, shell, agent = silent.split_session(
        agent_command(silent), agent_in_new_tab=True
    )
    attach_app(silent, session)
    dashboard = open_dashboard_over(silent, session, shell)

    silent.cli("send-key", "-b", dashboard, "Enter")
    wait_until(lambda: NO_ANSWER_HINT in silent.screen(dashboard))

    assert len(selects(silent)) == 1
    assert holds(lambda: silent.focused(session) == dashboard, 3)
    assert len(floating_layers(silent.window(session))) == 1


def test_unanswered_jump_within_the_tab_lands_and_closes(silent: RexServer):
    session, shell, agent = silent.split_session(agent_command(silent))
    attach_app(silent, session)
    dashboard = open_dashboard_over(silent, session, shell)

    silent.cli("send-key", "-b", dashboard, "Enter")
    wait_until(lambda: not floating_layers(silent.window(session)))

    assert silent.focused(session) == agent


def test_a_jump_from_the_layer_remembers_the_block_beneath_it(rex: RexServer):
    # The dashboard's own block sits in the window's floating layer and is
    # gone once the layer closes, so last-agent must not get it.
    session, shell, agent = rex.split_session(agent_command(rex))
    dashboard = open_dashboard_over(rex, session, shell)

    rex.cli("send-key", "-b", dashboard, "Enter")
    wait_until(lambda: not floating_layers(rex.window(session)))

    assert rex.focused(session) == agent
    settings = Path(rex.env["XDG_STATE_HOME"]) / "workmux/settings.json"
    assert json.loads(settings.read_text())["last_pane_id"] == shell
