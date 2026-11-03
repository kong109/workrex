"""Rex test environment: an isolated Rex server per test."""

import json
import shlex
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any, Optional

from .conftest import MuxEnvironment, make_env_script


REX_APP_BIN = (
    Path.home()
    / "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current"
    / "Rex Server.app/Contents/MacOS/rex"
)


def rex_binary() -> Optional[str]:
    found = shutil.which("rex")
    if found:
        return found
    if REX_APP_BIN.is_file():
        return str(REX_APP_BIN)
    return None


def rex_base_label(label: str) -> str:
    """A Rex window label without the status icon workmux puts in front."""
    _, separator, name = label.partition("\u2002")
    return name if separator else label


class RexEnvironment(MuxEnvironment):
    SHELL_FLAVOR = "com.superlogical.terminal.shell"

    def __init__(self, tmp_path: Path):
        super().__init__(tmp_path)

        rex = rex_binary()
        assert rex is not None, "rex CLI not found"
        self.rex = rex
        # macOS limits socket paths to 104 bytes; pytest tmp paths run longer.
        self.socket_dir = Path(tempfile.mkdtemp(prefix="rx", dir="/tmp"))
        self.server_url = f"unix://{self.socket_dir / 's.sock'}"
        self.state_dir = self.socket_dir / "state"
        self.server: Optional[subprocess.Popen] = None
        self.session_id: Optional[str] = None
        self.runner_block_id: Optional[str] = None

        for name in (
            "TMUX",
            "TMUX_PANE",
            "WEZTERM_PANE",
            "REX_BLOCK",
            "REX_SESSION",
            "REX_SERVER",
        ):
            self.env.pop(name, None)
        self.env["PATH"] = f"{Path(self.rex).parent}:{self.env['PATH']}"

    @property
    def backend_name(self) -> str:
        return "rex"

    def start_server(self) -> None:
        self.state_dir.mkdir()
        # A server on a custom --listen address does not set REX_SERVER in its
        # panes; they inherit the server's environment. Without it, rex in a
        # test pane reaches the user's own server.
        self.env["REX_SERVER"] = self.server_url
        log = open(self.socket_dir / "server.log", "w")
        self.server = subprocess.Popen(
            [
                self.rex,
                "server",
                "run",
                "--listen",
                self.server_url,
                "--state-dir",
                str(self.state_dir),
                "--restore=false",
                "--config",
                "/dev/null",
            ],
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        log.close()

        # The fixture's teardown never runs when setup raises, so a failed
        # start stops the server it launched here.
        try:
            deadline = time.monotonic() + 15
            while self.mux_command(["whoami", "--json"], check=False).returncode != 0:
                if self.server.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError(
                        "Rex test server did not start:\n"
                        + (self.socket_dir / "server.log").read_text()
                    )
                time.sleep(0.05)

            created = self.api(
                "session.create",
                {
                    "label": "test",
                    "initial_windows": [
                        {"window_label": "test", "layout": self._shell_layout()}
                    ],
                },
            )
            self.session_id = created["session_id"]
            self.runner_block_id = created["initial_windows"][0]["block_ids"][0]
        except BaseException:
            self.stop_server()
            raise

    def stop_server(self) -> None:
        if self.server is not None and self.server.poll() is None:
            self.server.terminate()
            try:
                self.server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.server.kill()
                self.server.wait(timeout=5)
        self.server = None
        shutil.rmtree(self.socket_dir, ignore_errors=True)

    def mux_command(
        self, args: list[str], check: bool = True
    ) -> subprocess.CompletedProcess[str]:
        return self.run_command(
            [self.rex, "--autostart=false", "-S", self.server_url] + args,
            check=check,
        )

    def api(self, method: str, args: Optional[dict] = None) -> dict:
        result = subprocess.run(
            [self.rex, "--autostart=false", "-S", self.server_url, "api", "call"]
            + [method, "-"],
            input=json.dumps(args or {}),
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(f"rex api call {method} failed: {result.stderr}")
        return json.loads(result.stdout or "{}")

    def block_call(self, block_id: str, method: str, args: Optional[dict] = None):
        result = subprocess.run(
            [self.rex, "--autostart=false", "-S", self.server_url, "block", "call"]
            + ["-b", block_id, method, "-"],
            input=json.dumps(args or {}),
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            return None
        return json.loads(result.stdout or "{}")

    def _shell_layout(self) -> dict:
        return {
            "block": {
                "flavor": self.SHELL_FLAVOR,
                "options": {
                    "cwd": str(self.tmp_path),
                    # Skip /usr/bin/login, which resets HOME to the real one.
                    "shell": "none",
                    "login": False,
                    "command": [self.env.get("SHELL", "/bin/zsh"), "-l"],
                },
            }
        }

    def views(self) -> list[dict]:
        """Session views, the test session first."""
        sessions = self.api("session.list")["sessions"]
        sessions.sort(key=lambda s: s["session_id"] != self.session_id)
        views = []
        for session in sessions:
            try:
                views.append(
                    self.api("session.view", {"session_id": session["session_id"]})
                )
            except RuntimeError:
                pass
        return views

    def _test_view(self) -> dict:
        return self.api("session.view", {"session_id": self.session_id})

    def _windows(self, label: str) -> list[tuple[str, dict]]:
        return [
            (view["session_id"], window)
            for view in self.views()
            for window in view.get("windows", [])
            if rex_base_label(window.get("label", "")) == label
        ]

    @staticmethod
    def _window_block(window: dict) -> Optional[str]:
        if window.get("focused_block_id"):
            return window["focused_block_id"]
        for layer in window.get("layers", []):
            for block in layer.get("blocks", []):
                return block["block_id"]
        return None

    def _active_window_block(self) -> str:
        """The test session's active window, as tmux resolves `test:`."""
        view = self._test_view()
        for window in view.get("windows", []):
            if window["window_id"] == view.get("active_window_id"):
                block = self._window_block(window)
                if block:
                    return block
        assert self.runner_block_id is not None, "Rex server has not been started"
        return self.runner_block_id

    def _target_block(self, target: str) -> str:
        if target.startswith("block:"):
            return target
        if target in ("test:", "test"):
            return self._active_window_block()
        for _, window in self._windows(target.rstrip(":")):
            block = self._window_block(window)
            if block:
                return block
        raise RuntimeError(f"Window '{target}' not found")

    def list_windows(self) -> list[str]:
        return [
            rex_base_label(w["label"]) for w in self._test_view().get("windows", [])
        ]

    def list_window_ids(self) -> list[str]:
        return [w["window_id"] for w in self._test_view().get("windows", [])]

    def capture_pane(self, window_name: str) -> Optional[str]:
        try:
            block = self._target_block(window_name)
        except RuntimeError:
            return None
        report = self.block_call(block, "format", {"format": "text", "trim": True})
        return None if report is None else report.get("content", "")

    def send_keys(self, target: str, text: str, enter: bool = True) -> None:
        block = self._target_block(target)
        data = text + ("\r" if enter else "")
        if self.block_call(block, "write", {"data": data}) is None:
            raise RuntimeError(f"Could not write to Rex block {block}")

    def run_shell_background(self, script: str) -> None:
        assert self.runner_block_id is not None, "Rex server has not been started"
        command = make_env_script(self, script, {})
        self.send_keys(self.runner_block_id, f"nohup {command} >/dev/null 2>&1 &")

    def set_session_env(self, key: str, value: str) -> None:
        self.send_keys("test:", f"export {key}={shlex.quote(value)}")

    def kill_window(self, window_name: str) -> None:
        windows = self._windows(window_name)
        if not windows:
            raise RuntimeError(f"Window '{window_name}' not found")
        for session_id, window in windows:
            self.api(
                "session.close_window",
                {"session_id": session_id, "window_id": window["window_id"]},
            )

    def get_current_window(self) -> Optional[str]:
        view = self._test_view()
        active = view.get("active_window_id")
        for window in view.get("windows", []):
            if window["window_id"] == active:
                return rex_base_label(window.get("label", ""))
        return None

    def select_window(self, window_name: str) -> None:
        windows = self._windows(window_name)
        if not windows:
            raise RuntimeError(f"Window '{window_name}' not found")
        session_id, window = windows[0]
        self.api(
            "session.focus_window",
            {"session_id": session_id, "window_id": window["window_id"]},
        )

    def new_window(self, name: Optional[str] = None) -> None:
        args: dict[str, Any] = {
            "session_id": self.session_id,
            "layout": self._shell_layout(),
        }
        if name:
            args["window_label"] = name
        self.api("session.new_window", args)

    def list_sessions(self) -> list[str]:
        return [s.get("label", "") for s in self.api("session.list")["sessions"]]

    def new_session(self, name: str) -> None:
        self.api(
            "session.create",
            {"label": name, "initial_windows": [{"layout": self._shell_layout()}]},
        )

    def list_windows_in_session(self, session: str) -> list[str]:
        return [
            rex_base_label(window.get("label", ""))
            for view in self.views()
            if view.get("label") == session
            for window in view.get("windows", [])
        ]

    def configure_default_shell(self, shell: str) -> None:
        self.env["SHELL"] = shell
