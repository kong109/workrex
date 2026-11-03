"""The sidebar under Rex, through the real CLI, daemon and TUI.

Each test runs against the isolated server the `mux_server` fixture owns.
Run with `--backend=rex`.
"""

import json
import os
import re
import signal
import subprocess
import time
from pathlib import Path
from typing import Optional

import pytest

from .conftest import poll_until
from .rex_env import RexEnvironment

SIDEBAR_LABEL = "workrex-sidebar"
SPINNER = re.compile("[⠀-⣿]")


@pytest.fixture
def rex(mux_server, workmux_exe_path: Path):
    if mux_server.backend_name != "rex":
        pytest.skip("Rex sidebar tests need --backend=rex")
    env: RexEnvironment = mux_server
    # Sidebar sockets live in TMPDIR and must fit macOS sockaddr_un.
    short_tmp = env.socket_dir / "t"
    short_tmp.mkdir()
    env.env["TMPDIR"] = str(short_tmp)
    env.env["WORKMUX_BACKEND"] = "rex"
    claude = env.fake_bin_dir / "claude"
    claude.write_text("#!/bin/sh\nexec sleep 100000\n")
    claude.chmod(0o755)
    env.workmux = workmux_exe_path
    yield env
    env.run_command([str(workmux_exe_path), "sidebar", "off"], check=False)


def cli(env: RexEnvironment, *args: str, extra_env: Optional[dict] = None):
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


def shell_layout(env: RexEnvironment) -> dict:
    return env._shell_layout()


def new_window(env: RexEnvironment, session_id: str, layout: Optional[dict] = None):
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


def view(env: RexEnvironment, session_id: str) -> dict:
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


def windows(env: RexEnvironment, session_id: str) -> dict:
    return {window["window_id"]: window for window in view(env, session_id)["windows"]}


def all_have_sidebars(env: RexEnvironment, session_id: str) -> bool:
    return all(sidebar_of(w) for w in windows(env, session_id).values())


def split_layout(env: RexEnvironment) -> dict:
    return {
        "split": {
            "direction": "horizontal",
            "ratio": 0.5,
            "before": shell_layout(env),
            "after": shell_layout(env),
        }
    }


def sidebar_files(env: RexEnvironment) -> Path:
    return env.home_path / ".local/state/workmux/sidebar"


def daemon_pid(env: RexEnvironment) -> Optional[int]:
    for path in sidebar_files(env).glob("rex-*-daemon.json"):
        pid = json.loads(path.read_text())["globals"].get("@workmux_sidebar_daemon_pid")
        if pid:
            return int(pid)
    return None


def capture(env: RexEnvironment, block_id: str) -> str:
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
    side = sidebar_of(window)
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
        ratios = [sidebar_of(w)["ratio"] for w in windows(env, env.session_id).values()]
        return all(abs(ratio - 0.4) < 0.02 for ratio in ratios)

    assert poll_until(followed, timeout=1.0, poll_interval=0.05)
    settings = json.loads((env.home_path / ".local/state/workmux/settings.json").read_text())
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
    assert root_split(after[split_window])["ratio"] == 0.5
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


def test_agent_status_reaches_the_sidebar_rows(rex):
    env = rex
    env.mux_command(["send", "-b", env.runner_block_id, "claude\n"])
    assert poll_until(
        lambda: (env.block_call(env.runner_block_id, "process") or {})
        .get("foreground", {})
        .get("name")
        == "sleep",
        timeout=5,
    )
    cli(env, "on")
    sidebar = sidebar_of(windows(env, env.session_id)[next(iter(windows(env, env.session_id)))])
    status_env = dict(env.env, REX_SESSION=env.session_id, REX_BLOCK=env.runner_block_id)

    def set_status(status: str) -> None:
        subprocess.run(
            [str(env.workmux), "set-window-status", status],
            env=status_env,
            cwd=env.tmp_path,
            check=True,
            capture_output=True,
        )

    set_status("working")
    assert poll_until(
        lambda: SPINNER.search(capture(env, sidebar["block_id"])) is not None,
        timeout=2.5,
    ), capture(env, sidebar["block_id"])

    set_status("waiting")
    assert poll_until(
        lambda: SPINNER.search(capture(env, sidebar["block_id"])) is None,
        timeout=2.5,
    ), capture(env, sidebar["block_id"])


def test_quitting_from_the_sidebar_turns_it_off(rex):
    env = rex
    new_window(env, env.session_id)
    cli(env, "on")
    sidebar = sidebar_of(next(iter(windows(env, env.session_id).values())))
    env.mux_command(["send", "-b", sidebar["block_id"], "q"])
    assert poll_until(
        lambda: "Quit sidebar?" in capture(env, sidebar["block_id"]), timeout=2
    )
    env.mux_command(["send", "-b", sidebar["block_id"], "y"])
    assert poll_until(
        lambda: all(sidebar_count(w) == 0 for w in windows(env, env.session_id).values()),
        timeout=3,
    )
