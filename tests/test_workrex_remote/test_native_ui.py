"""Native remote rows retain their configured identity in interactive views."""

import json
import shlex
from pathlib import Path

from ..conftest import poll_until
from .conftest import (
    LOOPBACK,
    agent_dirs_env,
    commit_project,
    wm,
    write_script,
)
from ..support.bin_name import hook_bin


def remote_input_agent(env, exe, repo, node):
    received = node.root / "received-input"
    agent = write_script(
        node.root / "claude",
        f"{hook_bin()} register-agent </dev/null\n"
        f"{hook_bin()} set-window-status waiting </dev/null\n"
        "stty -echo\n"
        "echo REMOTE-INPUT-READY\n"
        "while IFS= read -r line; do\n"
        f"  printf '%s\\n' \"$line\" >> {shlex.quote(str(received))}\n"
        "done\n",
    )
    commit_project(
        repo,
        {
            "agent": str(agent),
            "dashboard": {"commit": "/commit", "merge": "/merge"},
            "panes": [
                {"command": "echo FIRST-PANE"},
                {"command": "<agent>", "split": "horizontal"},
            ],
        },
    )
    wm(env, exe, repo, "add feat --remote lo")
    node.wait_for_agent(env, "feat", "REMOTE-INPUT-READY")
    workspace = node.workspace("feat")
    placement = workspace["execution"]["rex"]
    session = placement["sessions"]["0"]
    node.rex.api(
        "block.close", {"session_id": session, "block_id": placement["blocks"]["0"]}
    )

    def observed_rows():
        rows = json.loads(wm(env, exe, repo, "status --json").stdout)["agents"]
        return [row for row in rows if row.get("remote")]

    assert poll_until(lambda: len(observed_rows()) == 1, timeout=15)
    [row] = observed_rows()
    assert row["remote"]["pane"] == 1
    assert row["remote"]["attached"] is False
    return workspace, received, placement["blocks"]["1"]


def capture(env, block):
    return env.mux_command(["capture", "-b", block, "--trim", "--unwrap"]).stdout


def all_blocks(env):
    return {
        block["block_id"]
        for window in env.all_windows()
        for layer in window["layers"]
        for block in layer["blocks"]
    }


def test_dashboard_input_paste_and_kill_use_the_recorded_remote_pane(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    env, exe, repo = mux_server, workrex_exe_path, mux_repo_path
    workspace, received, remote_block = remote_input_agent(env, exe, repo, node)
    remote_session = workspace["execution"]["rex"]["sessions"]["1"]
    unrelated = env.create_window("unrelated", session=remote_session)
    [unrelated_block] = env.pane_ids(unrelated["window_id"])
    dashboard = env.create_window("dashboard", cwd=repo)
    [block] = env.pane_ids(dashboard["window_id"])
    command = shlex.join(
        [
            "env",
            *(
                f"{key}={value}"
                for key, value in {**LOOPBACK, **agent_dirs_env(env.home_path)}.items()
            ),
            str(exe),
            "dashboard",
        ]
    )
    env.send_keys(block, f"exec {command}")
    try:
        assert poll_until(lambda: "feat" in capture(env, block), timeout=15), capture(
            env, block
        )
        env.send_keys(block, "i", enter=False)
        assert poll_until(lambda: "INPUT MODE" in capture(env, block), timeout=5), (
            capture(env, block)
        )
        env.send_keys(block, "REMOTE-INPUT-42")
        assert poll_until(
            lambda: received.exists()
            and received.read_text().splitlines() == ["REMOTE-INPUT-42"],
            timeout=15,
        ), capture(env, block)
        env.send_keys(block, "\x1b[200~REMOTE-PASTE-43\x1b[201~")
        assert poll_until(
            lambda: received.read_text().splitlines()
            == ["REMOTE-INPUT-42", "REMOTE-PASTE-43"],
            timeout=15,
        ), capture(env, block)
        env.send_keys(block, "\x1b", enter=False)
        assert poll_until(lambda: "INPUT MODE" not in capture(env, block), timeout=5)
        expected = ["REMOTE-INPUT-42", "REMOTE-PASTE-43"]
        for key, command in [("c", "/commit"), ("m", "/merge")]:
            env.send_keys(block, key, enter=False)
            expected.append(command)
            assert poll_until(
                lambda: received.read_text().splitlines() == expected, timeout=15
            ), capture(env, block)
        (Path(workspace["worktree_path"]) / "ui-diff.txt").write_text(
            "remote diff only\n"
        )
        env.send_keys(block, "d", enter=False)
        assert poll_until(lambda: "ui-diff.txt" in capture(env, block), timeout=15), (
            capture(env, block)
        )
        env.send_keys(block, "c", enter=False)
        expected.append("/commit")
        assert poll_until(
            lambda: received.read_text().splitlines() == expected, timeout=15
        ), capture(env, block)
        assert poll_until(lambda: "WIP:" not in capture(env, block), timeout=5)
        env.send_keys(block, "X", enter=False)
        assert poll_until(lambda: remote_block not in all_blocks(env), timeout=15), (
            capture(env, block)
        )
        assert {env.runner_block_id, block, unrelated_block} <= all_blocks(env)
        local = capture(env, env.runner_block_id)
        assert "REMOTE-INPUT-42" not in local and "REMOTE-PASTE-43" not in local
        assert "/commit" not in local and "/merge" not in local
    finally:
        if env._windows(dashboard["window_id"]):
            env.kill_window(dashboard["window_id"])


def test_global_sidebar_routes_remote_rows_and_session_filter_excludes_them(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    env, exe, repo = mux_server, workrex_exe_path, mux_repo_path
    workspace, _, remote_block = remote_input_agent(env, exe, repo, node)
    remote_session = workspace["execution"]["rex"]["sessions"]["1"]
    temporary = node.root / "sidebar-tmp"
    temporary.mkdir()
    extra = {"TMPDIR": str(temporary)}

    def sidebar(command, **kwargs):
        return wm(env, exe, repo, "sidebar " + command, extra_env=extra, **kwargs)

    sidebar("on")
    try:
        sidebar("filter none")
        [block] = [
            block["block_id"]
            for window in env.session_view(env.session_id)["windows"]
            for layer in window["layers"]
            for block in layer["blocks"]
            if block.get("label") == "workrex-sidebar"
        ]
        assert poll_until(lambda: "feat" in capture(env, block), timeout=15), capture(
            env, block
        )
        before_local = env.window_view("test")["focused_block_id"]
        before_remote = node.view(remote_session)["windows"][0]["focused_block_id"]
        jumped = sidebar("jump 1", expect_fail=True)
        assert "Rex.app" in jumped.stderr
        assert env.window_view("test")["focused_block_id"] == before_local
        assert (
            node.view(remote_session)["windows"][0]["focused_block_id"] == before_remote
        )
        assert remote_block in all_blocks(env)
        sidebar("filter session")
        assert poll_until(lambda: "feat" not in capture(env, block), timeout=10), (
            capture(env, block)
        )
        sidebar("filter none")
        assert poll_until(lambda: "feat" in capture(env, block), timeout=10), capture(
            env, block
        )
    finally:
        sidebar("off")
