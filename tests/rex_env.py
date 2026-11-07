"""Rex test environment: an isolated Rex server per test."""

import json
import shlex
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

from .conftest import MuxEnvironment, make_env_script

REX_APP_BIN = (
    Path.home()
    / "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current"
    / "Rex Server.app/Contents/MacOS/rex"
)


def rex_binary() -> str | None:
    found = shutil.which("rex")
    if found:
        return found
    if REX_APP_BIN.is_file():
        return str(REX_APP_BIN)
    return None


def rex_base_label(label: str) -> str:
    """A Rex window label without the status icon workrex puts in front."""
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
        self.server_url = f"unix://{(self.socket_dir / 's.sock').resolve()}"
        self.state_dir = self.socket_dir / "state"
        self.server: subprocess.Popen | None = None
        self.session_id: str | None = None
        self.runner_block_id: str | None = None

        for name in (
            "REX_BLOCK",
            "REX_SESSION",
            "REX_SERVER",
            "REX_RUNTIME_DIR",
        ):
            self.env.pop(name, None)
        self.env["PATH"] = f"{Path(self.rex).parent}:{self.env['PATH']}"

        from .conftest import write_global_workrex_config

        write_global_workrex_config(self)

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

    def api(self, method: str, args: dict | None = None) -> dict:
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

    def block_call(self, block_id: str, method: str, args: dict | None = None):
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
        session_label, separator, window_label = label.lstrip("=").partition(":=")
        if not separator:
            session_label, window_label = "", label.lstrip("=")
        return [
            (view["session_id"], window)
            for view in self.views()
            if not session_label or view.get("label") == session_label
            for window in view.get("windows", [])
            if window.get("window_id") == label
            or rex_base_label(window.get("label", "")) == window_label
        ]

    def window_view(self, target: str) -> dict:
        matches = self._windows(target)
        if not matches:
            raise RuntimeError(f"Rex window {target!r} not found")
        return matches[0][1]

    def session_view(self, target: str) -> dict:
        for view in self.views():
            if target in [view["session_id"], view.get("label")]:
                return view
        raise RuntimeError(f"Rex session {target!r} not found")

    def all_windows(self) -> list[dict]:
        return [window for view in self.views() for window in view.get("windows", [])]

    def pane_ids(self, target: str) -> list[str]:
        window = self.window_view(target)
        return [
            block["block_id"]
            for layer in window.get("layers", [])
            for block in layer.get("blocks", [])
            if block.get("flavor") == self.SHELL_FLAVOR
        ]

    def rename_window(self, target: str, label: str) -> None:
        session, window = self._windows(target)[0]
        self.api(
            "session.set_window_label",
            {
                "session_id": session,
                "window_id": window["window_id"],
                "label": label,
            },
        )

    def rename_session(self, target: str, label: str) -> None:
        self.api(
            "session.set_label",
            {
                "session_id": self.session_view(target)["session_id"],
                "label": label,
            },
        )

    def create_window(
        self, label: str, cwd: Path | None = None, session: str | None = None
    ) -> dict:
        layout = self._shell_layout()
        if cwd is not None:
            layout["block"]["options"]["cwd"] = str(cwd)
        return self.api(
            "session.new_window",
            {
                "session_id": self.session_view(session)["session_id"]
                if session
                else self.session_id,
                "window_label": label,
                "layout": layout,
                "focus": False,
            },
        )

    def window_token(self, target: str) -> str:
        window_id = self.window_view(target)["window_id"]
        records = Path(self.env["XDG_STATE_HOME"]) / "workrex" / "rex-windows"
        for path in records.glob("*/*.json"):
            record = json.loads(path.read_text())
            if record.get("window_id") == window_id:
                return record["token"]
        return ""

    @staticmethod
    def _rows_result(
        rows: list[list[str]], separator: str = "\t"
    ) -> subprocess.CompletedProcess[str]:
        """Project structured live Rex observations for text-output assertions."""
        content = "\n".join(separator.join(row) for row in rows)
        return subprocess.CompletedProcess(
            ["rex", "api", "call"], 0, content + ("\n" if rows else ""), ""
        )

    def read_windows(
        self,
        fields: list[str],
        session: str | None = None,
        all_sessions: bool = False,
        separator: str = "\t",
    ) -> subprocess.CompletedProcess[str]:
        views = (
            self.views()
            if all_sessions
            else [
                self.session_view(session.rstrip(":")) if session else self._test_view()
            ]
        )
        rows = []
        for view in views:
            for window in view.get("windows", []):
                values = dict(window, label=rex_base_label(window.get("label", "")))
                if "token" in fields:
                    values["token"] = self.window_token(window["window_id"])
                rows.append([str(values.get(field, "")) for field in fields])
        return self._rows_result(rows, separator)

    def read_blocks(
        self, target: str, fields: list[str], separator: str = "\t"
    ) -> subprocess.CompletedProcess[str]:
        window = self.window_view(target)
        rows = []
        for index, block in enumerate(self.pane_ids(target)):
            report = self.block_call(block, "process", {}) or {}
            process = report.get("foreground") or report.get("child") or {}
            values = {
                "block_id": block,
                "command": process.get("name", ""),
                "pid": (report.get("child") or process).get("pid", ""),
                "cwd": self._process_cwd(report),
                "active": int(window.get("focused_block_id") == block),
                "index": index,
                "zoomed": int(window.get("zoomed_block_id") is not None),
            }
            rows.append([str(values.get(field, "")) for field in fields])
        return self._rows_result(rows, separator)

    @staticmethod
    def _process_cwd(report: dict) -> str:
        return next(
            (
                p["cwd"]
                for p in [
                    report.get("foreground"),
                    *report.get("ancestors", []),
                    report.get("child"),
                ]
                if p and p.get("cwd")
            ),
            "",
        )

    def read_current(
        self, field: str, target: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        target = (
            target if target is not None else str(self._test_view()["active_window_id"])
        )
        if field == "block_id":
            value = self._target_block(target)
        elif field == "cwd":
            value = self._process_cwd(
                self.block_call(self._target_block(target), "process", {}) or {}
            )
        elif field == "token":
            value = self.window_token(target)
        elif field == "session_id":
            value = self.session_view(target.lstrip("=").rstrip(":"))["session_id"]
        else:
            value = self.window_view(target).get(field, "")
        return self._rows_result([[str(value)]])

    def create_window_result(
        self,
        label: str,
        cwd: Path | None = None,
        session: str | None = None,
        field: str = "window_id",
    ) -> subprocess.CompletedProcess[str]:
        result = self.create_window(label, cwd, session)
        value = result["block_ids"][0] if field == "block_id" else result["window_id"]
        return self._rows_result([[value]])

    def read_sessions(self, field: str = "label") -> subprocess.CompletedProcess[str]:
        return self._rows_result(
            [[str(session[field])] for session in self.api("session.list")["sessions"]]
        )

    def session_result(
        self, target: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        sessions = self.api("session.list")["sessions"]
        found = (
            bool(sessions)
            if target is None
            else any(
                target.lstrip("=").rstrip(":")
                in [session["session_id"], session.get("label")]
                for session in sessions
            )
        )
        return subprocess.CompletedProcess(
            ["rex", "session", "list"], 0 if found else 1, "", ""
        )

    @staticmethod
    def _window_block(window: dict) -> str | None:
        if window.get("focused_block_id"):
            return window["focused_block_id"]
        for layer in window.get("layers", []):
            for block in layer.get("blocks", []):
                return block["block_id"]
        return None

    def _active_window_block(self) -> str:
        """The focused terminal block in the fixture session."""
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
        if target.startswith("=") and target.endswith(":"):
            view = self.session_view(target[1:-1])
            window = next(
                w for w in view["windows"] if w["window_id"] == view["active_window_id"]
            )
            block = self._window_block(window)
            if block:
                return block
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

    def capture_pane(self, window_name: str) -> str | None:
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

    def get_current_window(self) -> str | None:
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

    def new_window(self, name: str | None = None) -> None:
        args: dict[str, Any] = {
            "session_id": self.session_id,
            "layout": self._shell_layout(),
        }
        if name:
            args["window_label"] = name
        self.api("session.new_window", args)

    def list_sessions(self) -> list[str]:
        return [s.get("label", "") for s in self.api("session.list")["sessions"]]

    def new_session(self, name: str, cwd: Path | None = None) -> None:
        layout = self._shell_layout()
        if cwd is not None:
            layout["block"]["options"]["cwd"] = str(cwd)
        self.api(
            "session.create",
            {"label": name, "initial_windows": [{"layout": layout}]},
        )

    def list_windows_in_session(self, session: str) -> list[str]:
        return [
            rex_base_label(window.get("label", ""))
            for view in self.views()
            if view.get("label") == session
            for window in view.get("windows", [])
        ]

    def session_pane_path(self, session: str) -> str:
        view = next(v for v in self.views() if v.get("label") == session)
        window = next(
            w for w in view["windows"] if w["window_id"] == view["active_window_id"]
        )
        block = window.get("focused_block_id") or self._window_block(window)
        if block is None:
            return ""
        report = self.block_call(block, "process", {}) or {}
        processes = [
            report.get("foreground"),
            *report.get("ancestors", []),
            report.get("child"),
        ]
        return next((p["cwd"] for p in processes if p and p.get("cwd")), "")

    def configure_default_shell(self, shell: str) -> None:
        self.env["SHELL"] = shell
