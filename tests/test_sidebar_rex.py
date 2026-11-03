"""The sidebar under Rex, through the real CLI, daemon and TUI.

Each test runs against the isolated server the `mux_server` fixture owns.
Run with `--backend=rex`.
"""

import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

import pytest

from .conftest import get_test_backends, poll_until, skip_if_backend_unavailable
from .rex_env import RexEnvironment

SIDEBAR_LABEL = "workrex-sidebar"
SPINNER = re.compile("[⠀-⣿]")


class SidebarRex(RexEnvironment):
    def __init__(self, tmp_path: Path, workmux: Path):
        super().__init__(tmp_path)
        self.workmux = workmux


@pytest.fixture
def rex(request, tmp_path: Path, workmux_exe_path: Path):
    if "rex" not in get_test_backends(request.config):
        pytest.skip("Rex sidebar tests need --backend=rex")
    skip_if_backend_unavailable("rex")
    env = SidebarRex(tmp_path, workmux_exe_path)
    env.start_server()
    # Sidebar sockets live in TMPDIR and must fit macOS sockaddr_un.
    short_tmp = env.socket_dir / "t"
    short_tmp.mkdir()
    env.env["TMPDIR"] = str(short_tmp)
    env.env["WORKMUX_BACKEND"] = "rex"
    claude = env.fake_bin_dir / "claude"
    claude.write_text("#!/bin/sh\nexec sleep 100000\n")
    claude.chmod(0o755)
    yield env
    env.run_command([str(workmux_exe_path), "sidebar", "off"], check=False)
    env.stop_server()


def cli(env: SidebarRex, *args: str, extra_env: Optional[dict] = None):
    run_env = dict(env.env, **(extra_env or {}))
    result = subprocess.run(
        [str(env.workmux), "sidebar", *args],
        cwd=env.tmp_path,
        env=run_env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return result


def shell_layout(env: SidebarRex) -> dict:
    return env._shell_layout()


def new_window(env: SidebarRex, session_id: str, layout: Optional[dict] = None):
    created = env.api(
        "session.new_window",
        {
            "session_id": session_id,
            "window_label": "extra",
            "layout": layout or shell_layout(env),
            "focus": False,
        },
    )
    return created["window_id"]


def view(env: SidebarRex, session_id: str) -> dict:
    return env.api("session.view", {"session_id": session_id})


def tiled(window: dict) -> dict:
    return next(layer for layer in window["layers"] if layer["kind"] == "tiled")


def root_split(window: dict) -> Optional[dict]:
    return tiled(window)["layout"].get("split")


def sidebar_of(window: dict) -> Optional[dict]:
    """The window's sidebar block, when it sits on the left of the root split."""
    split = root_split(window)
    if split is None:
        return None
    before = split["before"].get("block_id")
    for block in tiled(window)["blocks"]:
        if block["block_id"] == before and block.get("label") == SIDEBAR_LABEL:
            return {"block_id": before, "ratio": split["ratio"], "split": split}
    return None


def placed_sidebar(window: dict) -> dict:
    sidebar = sidebar_of(window)
    assert sidebar is not None, window
    return sidebar


def sidebar_count(window: dict) -> int:
    return sum(
        1
        for layer in window["layers"]
        for block in layer["blocks"]
        if block.get("label") == SIDEBAR_LABEL
    )


def first_block(node: dict) -> str:
    while node.get("split"):
        node = node["split"]["before"]
    return node["block_id"]


def windows(env: SidebarRex, session_id: str) -> dict:
    return {window["window_id"]: window for window in view(env, session_id)["windows"]}


def all_have_sidebars(env: SidebarRex, session_id: str) -> bool:
    return all(sidebar_of(w) for w in windows(env, session_id).values())


def split_layout(env: SidebarRex) -> dict:
    return {
        "split": {
            "direction": "horizontal",
            "ratio": 0.5,
            "before": shell_layout(env),
            "after": shell_layout(env),
        }
    }


def sidebar_files(env: SidebarRex) -> Path:
    return env.home_path / ".local/state/workmux/sidebar"


def daemon_pid(env: SidebarRex) -> Optional[int]:
    for path in sidebar_files(env).glob("rex-*-daemon.json"):
        pid = json.loads(path.read_text())["globals"].get("@workmux_sidebar_daemon_pid")
        if pid:
            return int(pid)
    return None


def capture(env: SidebarRex, block_id: str) -> str:
    return env.mux_command(["capture", "-b", block_id]).stdout


def test_on_adds_a_left_sidebar_to_every_window_without_moving_focus(rex):
    env = rex
    new_window(env, env.session_id, split_layout(env))
    before = view(env, env.session_id)

    cli(env, "on")

    after = view(env, env.session_id)
    assert after["active_window_id"] == before["active_window_id"]
    focused = {w["window_id"]: w.get("focused_block_id") for w in before["windows"]}
    for window in after["windows"]:
        assert sidebar_of(window), window
        assert sidebar_count(window) == 1
        assert window.get("focused_block_id") == focused[window["window_id"]]


def test_a_new_window_gets_a_sidebar_from_its_creation_event(rex):
    env = rex
    cli(env, "on")
    started = time.monotonic()
    window_id = new_window(env, env.session_id)
    assert poll_until(
        lambda: sidebar_of(windows(env, env.session_id)[window_id]) is not None,
        timeout=1.0,
        poll_interval=0.02,
    ), "no sidebar within 1 s"
    print(f"new window to sidebar: {(time.monotonic() - started) * 1000:.0f} ms")
    assert sidebar_count(windows(env, env.session_id)[window_id]) == 1


def test_dragging_one_sidebar_moves_every_sidebar_and_persists(rex):
    env = rex
    new_window(env, env.session_id)
    cli(env, "on")
    assert poll_until(lambda: all_have_sidebars(env, env.session_id), timeout=2)
    window = next(iter(windows(env, env.session_id).values()))
    side = placed_sidebar(window)
    env.api(
        "session.resize_pane",
        {
            "session_id": env.session_id,
            "direction": "horizontal",
            "before_block_id": side["block_id"],
            "after_block_id": first_block(side["split"]["after"]),
            "ratio": 0.4,
        },
    )

    def followed() -> bool:
        ratios = [
            placed_sidebar(w)["ratio"] for w in windows(env, env.session_id).values()
        ]
        return all(abs(ratio - 0.4) < 0.02 for ratio in ratios)

    assert poll_until(followed, timeout=1.0, poll_interval=0.05)
    settings = json.loads(
        (env.home_path / ".local/state/workmux/settings.json").read_text()
    )
    # Headless blocks are 80 columns wide.
    assert settings["sidebar_width"] == 32


def test_off_restores_every_window_layout(rex):
    env = rex
    split_window = new_window(env, env.session_id, split_layout(env))
    before = windows(env, env.session_id)

    cli(env, "on")
    assert all_have_sidebars(env, env.session_id)
    pid = daemon_pid(env)
    assert pid is not None
    cli(env, "off")

    after = windows(env, env.session_id)
    for window_id, window in after.items():
        assert sidebar_count(window) == 0
        old = root_split(before[window_id])
        new = root_split(window)
        assert (old or {}).get("split_id") == (new or {}).get("split_id")
    split = root_split(after[split_window])
    assert split is not None and split["ratio"] == 0.5
    assert poll_until(lambda: not _alive(pid), timeout=2), "daemon still running"


def test_session_scope_covers_only_that_session(rex):
    env = rex
    other = env.api(
        "session.create",
        {
            "label": "other",
            "initial_windows": [{"window_label": "o", "layout": shell_layout(env)}],
        },
    )["session_id"]
    inside = {"REX_SESSION": env.session_id, "REX_BLOCK": env.runner_block_id}

    cli(env, "--session", "on", extra_env=inside)

    assert all_have_sidebars(env, env.session_id)
    assert all(sidebar_count(w) == 0 for w in windows(env, other).values())
    other_window = new_window(env, other)
    own_window = new_window(env, env.session_id)
    assert poll_until(
        lambda: sidebar_of(windows(env, env.session_id)[own_window]) is not None,
        timeout=1.0,
    )
    time.sleep(0.5)
    assert sidebar_count(windows(env, other)[other_window]) == 0

    cli(env, "--session", "off", extra_env=inside)
    assert all(sidebar_count(w) == 0 for w in windows(env, env.session_id).values())


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def test_a_killed_daemon_is_restarted_by_the_next_command(rex):
    env = rex
    cli(env, "on")
    pid = daemon_pid(env)
    assert pid is not None
    command = subprocess.run(
        ["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True
    ).stdout
    assert "_sidebar-daemon" in command, command
    os.kill(pid, signal.SIGKILL)
    assert poll_until(lambda: not _alive(pid), timeout=2)

    cli(env, "on")

    restarted = daemon_pid(env)
    assert restarted is not None and restarted != pid and _alive(restarted)
    window_id = new_window(env, env.session_id)
    assert poll_until(
        lambda: sidebar_of(windows(env, env.session_id)[window_id]) is not None,
        timeout=1.0,
    )


def test_the_daemon_exits_after_its_server_dies(rex):
    env = rex
    cli(env, "on")
    pid = daemon_pid(env)
    assert pid is not None and _alive(pid)

    env.server.terminate()
    env.server.wait(timeout=5)

    # The event stream ends and reconnecting finds no server.
    assert poll_until(lambda: not _alive(pid), timeout=20, poll_interval=0.2), (
        "daemon outlived its server"
    )


def events_children(pid: int) -> list:
    rows = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,command="], capture_output=True, text=True
    ).stdout
    return [
        int(fields[0])
        for fields in (row.split(None, 2) for row in rows.splitlines())
        if len(fields) == 3 and int(fields[1]) == pid and " events " in fields[2]
    ]


def test_a_window_made_before_the_server_stalls_still_gets_its_sidebar(rex):
    env = rex
    cli(env, "on")
    window_id = new_window(env, env.session_id)
    # Stop the server after it publishes the creation event but inside the
    # daemon's 50 ms batch window, so the pass for the new window times out.
    time.sleep(0.02)
    os.kill(env.server.pid, signal.SIGSTOP)
    try:
        time.sleep(11)
    finally:
        os.kill(env.server.pid, signal.SIGCONT)
    assert poll_until(
        lambda: sidebar_of(windows(env, env.session_id)[window_id]) is not None,
        timeout=36,
        poll_interval=0.5,
    ), "the failed pass dropped the new window"


def test_a_sidebar_starting_as_the_server_stalls_outlasts_the_stall(rex):
    env = rex
    cli(env, "on")
    window_id = new_window(env, env.session_id)
    deadline = time.monotonic() + 10
    while sidebar_of(windows(env, env.session_id)[window_id]) is None:
        assert time.monotonic() < deadline, "the new window got no sidebar"
        time.sleep(0.01)
    # The sidebar TUI is still starting. Stopping the server past the rex
    # CLI's 10 s timeout fails its first calls; a sidebar that quits then
    # leaves its window looking closed by the user.
    os.kill(env.server.pid, signal.SIGSTOP)
    try:
        time.sleep(14)
    finally:
        os.kill(env.server.pid, signal.SIGCONT)
    time.sleep(5)
    assert sidebar_of(windows(env, env.session_id)[window_id]) is not None, (
        "the sidebar quit during the stall and stayed closed"
    )


def test_a_closed_sidebar_stays_closed_when_the_event_stream_restarts(rex):
    env = rex
    kept = new_window(env, env.session_id)
    cli(env, "on")
    closed = next(window for window in windows(env, env.session_id) if window != kept)
    sidebar = placed_sidebar(windows(env, env.session_id)[closed])
    env.api(
        "block.close",
        {"session_id": env.session_id, "block_id": sidebar["block_id"]},
    )
    assert poll_until(
        lambda: sidebar_count(windows(env, env.session_id)[closed]) == 0, timeout=2
    )
    time.sleep(0.5)

    pid = daemon_pid(env)
    assert pid is not None
    [stream] = events_children(pid)
    os.kill(stream, signal.SIGKILL)
    assert poll_until(lambda: events_children(pid) not in ([], [stream]), timeout=10), (
        "the event stream did not restart"
    )
    later = new_window(env, env.session_id)
    assert poll_until(
        lambda: sidebar_of(windows(env, env.session_id)[later]) is not None, timeout=2
    )

    current = windows(env, env.session_id)
    assert sidebar_count(current[closed]) == 0, "the closed sidebar came back"
    assert sidebar_of(current[kept]) is not None

    cli(env, "on")
    assert sidebar_of(windows(env, env.session_id)[closed]) is not None


def start_agent(env: SidebarRex) -> None:
    block = env.runner_block_id
    assert block is not None
    env.mux_command(["send", "-b", block, "claude\n"])
    assert poll_until(
        lambda: (env.block_call(block, "process") or {})
        .get("foreground", {})
        .get("name")
        == "sleep",
        timeout=5,
    )


def set_status(env: SidebarRex, status: str) -> None:
    assert env.session_id is not None and env.runner_block_id is not None
    subprocess.run(
        [str(env.workmux), "set-window-status", status],
        env=dict(env.env, REX_SESSION=env.session_id, REX_BLOCK=env.runner_block_id),
        cwd=env.tmp_path,
        check=True,
        capture_output=True,
    )


def test_agent_status_reaches_the_sidebar_rows(rex):
    env = rex
    start_agent(env)
    cli(env, "on")
    sidebar = placed_sidebar(next(iter(windows(env, env.session_id).values())))

    set_status(env, "working")
    assert poll_until(
        lambda: SPINNER.search(capture(env, sidebar["block_id"])) is not None,
        timeout=2.5,
    ), capture(env, sidebar["block_id"])

    set_status(env, "waiting")
    assert poll_until(
        lambda: SPINNER.search(capture(env, sidebar["block_id"])) is None,
        timeout=2.5,
    ), capture(env, sidebar["block_id"])


def test_quitting_from_the_sidebar_turns_it_off(rex):
    env = rex
    new_window(env, env.session_id)
    cli(env, "on")
    sidebar = placed_sidebar(next(iter(windows(env, env.session_id).values())))
    env.mux_command(["send", "-b", sidebar["block_id"], "q"])
    assert poll_until(
        lambda: "Quit sidebar?" in capture(env, sidebar["block_id"]), timeout=2
    )
    env.mux_command(["send", "-b", sidebar["block_id"], "y"])
    assert poll_until(
        lambda: all(
            sidebar_count(w) == 0 for w in windows(env, env.session_id).values()
        ),
        timeout=3,
    )


REMOTE_CONTROL_FIX = (
    "Rex.app did not switch: Remote Control is off. Turn it on in Rex Settings."
)

# A `rex` that reports one Rex.app client, which owns every block and shows
# the test session, and answers its `client.perform` the way Rex.app does with
# Remote Control off. Every other call goes to the real `rex`.
REFUSING_APP_REX = r"""#!{python} -I
import json, os, subprocess, sys

real, app, session = {real!r}, "client:fake-app", {session!r}
args = sys.argv[1:]
if "client.perform" in args:
    sys.stderr.write(
        'Error: stream response "control-1" failed: Rex.app is not accepting '
        "actions. Turn on Remote Control in its Settings under Rex Server. "
        "(status=403 code=refused)\n"
    )
    sys.exit(1)
if "client.list" not in args and "size" not in args:
    os.execv(real, [real, *args])
stdin = sys.stdin.read() if args[-1:] == ["-"] else None
done = subprocess.run([real, *args], input=stdin, capture_output=True, text=True)
if done.returncode:
    sys.stderr.write(done.stderr)
    sys.exit(done.returncode)
reply = json.loads(done.stdout)
if "client.list" in args:
    reply.setdefault("clients", []).append(
        {{
            "client_id": app,
            "info": {{"kind": "app"}},
            "connection_state": "connected",
            "session_ids": [session],
        }}
    )
else:
    reply["owner"] = app
print(json.dumps(reply))
"""


def refusing_app(env: SidebarRex) -> None:
    shim = env.tmp_path / "rex-shim"
    shim.mkdir()
    rex = shim / "rex"
    rex.write_text(
        REFUSING_APP_REX.format(
            python=sys.executable, real=env.rex, session=env.session_id
        )
    )
    rex.chmod(0o755)
    env.env["REX_BIN_DIR"] = str(shim)


def wrapped_text(screen: str) -> str:
    return " ".join(line.strip() for line in screen.splitlines() if line.strip())


def test_a_refused_jump_shows_the_fix_in_the_sidebar_until_a_key(rex):
    env = rex
    refusing_app(env)
    start_agent(env)
    set_status(env, "waiting")
    cli(env, "on")
    window = next(iter(windows(env, env.session_id).values()))
    sidebar = placed_sidebar(window)["block_id"]
    assert poll_until(lambda: "· main" in capture(env, sidebar), timeout=3), capture(
        env, sidebar
    )

    def held() -> bool:
        return REMOTE_CONTROL_FIX in wrapped_text(capture(env, sidebar))

    def refuse() -> float:
        env.mux_command(["send-key", "-b", sidebar, "enter"])
        assert poll_until(held, timeout=5), capture(env, sidebar)
        return time.monotonic()

    def sleep_until(at: float) -> None:
        time.sleep(max(0.0, at - time.monotonic()))

    shown = refuse()
    sleep_until(shown + 5.5)
    assert held(), "the fix went without a key"
    env.mux_command(["send-key", "-b", sidebar, "j"])
    assert poll_until(
        lambda: "Remote Control" not in capture(env, sidebar), timeout=2
    ), capture(env, sidebar)

    # No snapshot comes on a quiet sidebar, so the fix goes at its deadline.
    shown = refuse()
    sleep_until(shown + 1)
    env.mux_command(["send-key", "-b", sidebar, "k"])
    sleep_until(shown + 4.5)
    assert held(), "a key inside 5 s cleared the fix"
    assert poll_until(
        lambda: "Remote Control" not in capture(env, sidebar),
        timeout=shown + 6.5 - time.monotonic(),
    ), capture(env, sidebar)

    jump = subprocess.run(
        [str(env.workmux), "sidebar", "jump", "1"],
        env=dict(env.env, REX_SESSION=env.session_id, REX_BLOCK=env.runner_block_id),
        cwd=env.tmp_path,
        capture_output=True,
        text=True,
    )
    assert jump.returncode == 1
    assert jump.stderr.count(REMOTE_CONTROL_FIX) == 1, jump.stderr


def test_an_agent_that_exits_leaves_the_running_sidebar(rex):
    env = rex
    go = env.tmp_path / "go"
    made = json.loads(
        env.mux_command(
            ["run", "-s", env.session_id, "--keep-open", "--shell=none"]
            + ["--focus=false", "-c", str(env.tmp_path), "--json", "--"]
            + ["/bin/sh", "-c", f"while [ ! -e {go} ]; do sleep 0.1; done; exit 3"]
        ).stdout
    )
    agent = made["block_ids"][0]
    subprocess.run(
        [str(env.workmux), "set-window-status", "working"],
        env=dict(env.env, REX_SESSION=env.session_id, REX_BLOCK=agent),
        cwd=env.tmp_path,
        check=True,
        capture_output=True,
    )
    cli(env, "on")
    window = windows(env, env.session_id)[made["window_id"]]
    sidebar = placed_sidebar(window)["block_id"]
    assert poll_until(
        lambda: SPINNER.search(capture(env, sidebar)) is not None, timeout=3
    ), capture(env, sidebar)

    go.touch()
    assert poll_until(
        lambda: "last_exit" in (env.block_call(agent, "process") or {}), timeout=5
    )
    # The exit wakes the daemon; without the wake the row waits for the
    # next 2 s observation.
    assert poll_until(
        lambda: SPINNER.search(capture(env, sidebar)) is None, timeout=1
    ), capture(env, sidebar)


def test_a_sidebar_started_without_a_server_exits_at_once(rex):
    env = rex
    run_env = dict(
        env.env,
        REX_SERVER=f"unix://{env.socket_dir / 'missing.sock'}",
        REX_SESSION=env.session_id,
        REX_BLOCK=env.runner_block_id,
    )
    done = subprocess.run(
        [str(env.workmux), "_sidebar-run"],
        env=run_env,
        cwd=env.tmp_path,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        timeout=3,
    )
    assert done.returncode == 0, done.stderr


def closed_windows(env: SidebarRex) -> dict:
    [daemon] = sidebar_files(env).glob("rex-*-daemon.json")
    return {
        window: options["@workmux_sidebar_closed"]
        for window, options in json.loads(daemon.read_text()).get("windows", {}).items()
        if "@workmux_sidebar_closed" in options
    }


def test_a_closed_sidebar_outlasts_a_quit_elsewhere_and_goes_with_its_window(rex):
    env = rex
    other = env.api(
        "session.create",
        {
            "label": "other",
            "initial_windows": [{"window_label": "o", "layout": shell_layout(env)}],
        },
    )["session_id"]
    closing = new_window(env, other)
    for session, block in [
        (env.session_id, env.runner_block_id),
        (other, first_block(tiled(windows(env, other)[closing])["layout"])),
    ]:
        cli(
            env,
            "--session",
            "on",
            extra_env={"REX_SESSION": session, "REX_BLOCK": block},
        )
    sidebar = placed_sidebar(windows(env, other)[closing])["block_id"]
    env.api("block.close", {"session_id": other, "block_id": sidebar})
    assert poll_until(lambda: closing in closed_windows(env), timeout=3), (
        "the daemon did not record the closed sidebar"
    )

    own = placed_sidebar(next(iter(windows(env, env.session_id).values())))
    env.mux_command(["send", "-b", own["block_id"], "q"])
    assert poll_until(
        lambda: "Quit sidebar?" in capture(env, own["block_id"]), timeout=2
    )
    env.mux_command(["send", "-b", own["block_id"], "y"])
    assert poll_until(
        lambda: all(
            sidebar_count(w) == 0 for w in windows(env, env.session_id).values()
        ),
        timeout=3,
    )
    after = windows(env, other)
    assert sidebar_count(after[closing]) == 0, "the quit reopened a closed sidebar"
    assert all(sidebar_of(w) for window_id, w in after.items() if window_id != closing)

    env.api("session.close_window", {"session_id": other, "window_id": closing})
    assert poll_until(lambda: closing not in closed_windows(env), timeout=3), (
        "the closed record outlived its window"
    )
