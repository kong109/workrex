"""Agent status from Rex program status (OSC 7501), falling back to hooks."""

import json
import shutil
import subprocess
import textwrap
import time
from pathlib import Path
from typing import Optional

import pytest

from .conftest import (
    MuxEnvironment,
    get_window_name,
    get_worktree_path,
    poll_until,
    run_workmux_add,
    wait_for_window_ready,
    write_workmux_config,
)
from .rex_env import RexEnvironment
from .test_sidebar_rex import SIDEBAR_LABEL, SPINNER

FAKE_AGENT = textwrap.dedent(
    """
    import sys
    # Each input line is an OSC 7501 body such as state=working:app=fake.
    print("fake agent ready", flush=True)
    for line in sys.stdin:
        line = line.strip()
        if line == "exit":
            sys.exit(0)
        if line:
            sys.stdout.write("\\x1b]7501;" + line + "\\x1b\\\\")
            print("reported " + line, flush=True)
    """
)


@pytest.fixture
def rex(mux_server: MuxEnvironment) -> RexEnvironment:
    if not isinstance(mux_server, RexEnvironment):
        pytest.skip("program status is a Rex feature")
    return mux_server


def workmux(env: RexEnvironment, exe: Path, cwd: Path, *args: str, block=None):
    run_env = {**env.env, "WORKMUX_BACKEND": "rex"}
    if block:
        run_env["REX_BLOCK"] = block
    return subprocess.run(
        [str(exe), *args], cwd=cwd, env=run_env, capture_output=True, text=True
    )


def agent_status(
    env: RexEnvironment, exe: Path, repo: Path, block: str
) -> Optional[str]:
    result = workmux(env, exe, repo, "status", "--json")
    assert result.returncode == 0, result.stderr
    for agent in json.loads(result.stdout)["agents"]:
        if agent["pane_id"] == block:
            return agent["status"]
    return None


def start_fake_agent(
    env: RexEnvironment,
    exe: Path,
    repo: Path,
    branch: str,
    launch: str = "exec {agent}",
    before=None,
) -> str:
    """Start the fake agent in a new worktree window. `launch` is the shell
    command line, and `before` runs on the block before the agent starts."""
    write_workmux_config(repo, panes=[{"focus": True}])
    run_workmux_add(env, exe, repo, branch)
    window = get_window_name(branch)
    wait_for_window_ready(env, window)
    block = env._target_block(window)
    if before:
        before(block)
    script = env.tmp_path / "fake_agent.py"
    script.write_text(FAKE_AGENT)
    env.send_keys(block, launch.format(agent=f"python3 -I {script}"))
    assert poll_until(
        lambda: "fake agent ready" in (env.capture_pane(block) or ""), timeout=10
    ), env.capture_pane(block)
    return block


def foreground_name(env: RexEnvironment, block: str) -> str:
    process = env.block_call(block, "process") or {}
    return (process.get("foreground") or {}).get("name", "")


def report(env: RexEnvironment, block: str, body: str) -> None:
    env.send_keys(block, body)
    assert poll_until(
        lambda: f"reported {body}" in (env.capture_pane(block) or ""), timeout=5
    ), env.capture_pane(block)


def window_label(env: RexEnvironment, block: str) -> str:
    for view in env.views():
        for window in view.get("windows", []):
            for layer in window.get("layers", []):
                if any(b["block_id"] == block for b in layer.get("blocks", [])):
                    return window["label"]
    raise AssertionError(f"no window holds {block}")


def test_program_status_shows_without_hooks(rex, workmux_exe_path, repo_path):
    block = start_fake_agent(rex, workmux_exe_path, repo_path, "osc-agent")
    assert agent_status(rex, workmux_exe_path, repo_path, block) is None

    for body, expected in [
        ("state=working:app=fake", "working"),
        ("state=blocked:kind=permission:app=fake", "waiting"),
        ("state=done:app=fake", "done"),
        ("state=error:app=fake", "done"),
        ("state=idle:app=fake", None),
    ]:
        report(rex, block, body)
        assert agent_status(rex, workmux_exe_path, repo_path, block) == expected, body


def test_agent_is_dropped_after_it_exits(rex, workmux_exe_path, repo_path):
    block = start_fake_agent(rex, workmux_exe_path, repo_path, "osc-exit")
    report(rex, block, "state=done:app=fake")
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "done"

    rex.send_keys(block, "exit")
    assert poll_until(
        lambda: agent_status(rex, workmux_exe_path, repo_path, block) is None,
        timeout=10,
    )


def test_program_status_wins_over_a_stale_hook_status(rex, workmux_exe_path, repo_path):
    block = start_fake_agent(rex, workmux_exe_path, repo_path, "osc-hooked")
    hook = workmux(
        rex, workmux_exe_path, repo_path, "set-window-status", "waiting", block=block
    )
    assert hook.returncode == 0, hook.stderr
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "waiting"

    report(rex, block, "state=working:app=fake")
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "working"

    report(rex, block, "state=clear")
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "waiting"


def test_hook_status_puts_its_icon_in_the_window_label(
    rex, workmux_exe_path, repo_path
):
    block = start_fake_agent(rex, workmux_exe_path, repo_path, "label-icon")
    window = get_window_name("label-icon")

    for status, icon in [("waiting", "💬"), ("done", "✅")]:
        hook = workmux(
            rex, workmux_exe_path, repo_path, "set-window-status", status, block=block
        )
        assert hook.returncode == 0, hook.stderr
        assert window_label(rex, block) == f"{icon}\u2002{window}"

    hook = workmux(
        rex, workmux_exe_path, repo_path, "set-window-status", "clear", block=block
    )
    assert hook.returncode == 0, hook.stderr
    assert window_label(rex, block) == window


def test_a_window_prefix_ending_in_a_space_keeps_its_window(
    rex, workmux_exe_path, repo_path
):
    prefix = " "
    write_workmux_config(repo_path, panes=[{"focus": True}], window_prefix=prefix)
    run_workmux_add(rex, workmux_exe_path, repo_path, "nerd")
    wait_for_window_ready(rex, f"{prefix}nerd")
    block = rex._target_block(f"{prefix}nerd")

    hook = workmux(
        rex, workmux_exe_path, repo_path, "set-window-status", "waiting", block=block
    )
    assert hook.returncode == 0, hook.stderr
    assert window_label(rex, block) == f"💬 {prefix}nerd"


def test_agent_run_from_the_shell_is_dropped_after_it_exits(
    rex, workmux_exe_path, repo_path
):
    block = start_fake_agent(
        rex, workmux_exe_path, repo_path, "osc-shell-exit", launch="{agent}"
    )
    report(rex, block, "state=done:app=fake")
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "done"

    rex.send_keys(block, "exit")
    assert poll_until(lambda: foreground_name(rex, block) == "zsh", timeout=10)
    assert agent_status(rex, workmux_exe_path, repo_path, block) is None


def test_a_shell_that_execs_after_the_agent_does_not_inherit_its_status(
    rex, workmux_exe_path, repo_path
):
    block = start_fake_agent(
        rex,
        workmux_exe_path,
        repo_path,
        "osc-sh-exec",
        launch="exec sh -c '{agent}; exec /bin/zsh -f'",
    )
    report(rex, block, "state=done:app=fake")
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "done"

    rex.send_keys(block, "exit")
    assert poll_until(lambda: foreground_name(rex, block) == "zsh", timeout=10)
    assert agent_status(rex, workmux_exe_path, repo_path, block) is None


def test_the_hook_status_returns_after_clear_even_after_pruning_readers(
    rex, workmux_exe_path, repo_path
):
    block = start_fake_agent(
        rex, workmux_exe_path, repo_path, "osc-clear", launch="{agent}"
    )
    hook = workmux(
        rex, workmux_exe_path, repo_path, "set-window-status", "waiting", block=block
    )
    assert hook.returncode == 0, hook.stderr

    report(rex, block, "state=working:app=fake")
    pruning = workmux(rex, workmux_exe_path, repo_path, "list")
    assert pruning.returncode == 0, pruning.stderr
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "working"

    report(rex, block, "state=clear")
    pruning = workmux(rex, workmux_exe_path, repo_path, "list")
    assert pruning.returncode == 0, pruning.stderr
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "waiting"


def test_a_stale_hook_file_does_not_block_adoption(rex, workmux_exe_path, repo_path):
    def shell_hook(block):
        hook = workmux(
            rex,
            workmux_exe_path,
            repo_path,
            "set-window-status",
            "waiting",
            block=block,
        )
        assert hook.returncode == 0, hook.stderr

    block = start_fake_agent(
        rex,
        workmux_exe_path,
        repo_path,
        "osc-stale-hook",
        launch="{agent}",
        before=shell_hook,
    )
    report(rex, block, "state=working:app=fake")
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "working"


def assert_last_done_focuses(env: RexEnvironment, exe: Path, repo: Path, block: str):
    def active_block():
        view = env.api("session.view", {"session_id": env.session_id})
        active = view.get("active_window_id")
        window = next(w for w in view["windows"] if w["window_id"] == active)
        return window.get("focused_block_id")

    env.api(
        "session.focus_block",
        {"session_id": env.session_id, "block_id": env.runner_block_id},
    )
    assert active_block() == env.runner_block_id

    result = workmux(env, exe, repo, "last-done")
    assert result.returncode == 0, result.stderr
    assert poll_until(lambda: active_block() == block, timeout=5), result.stdout


def test_last_done_focuses_an_agent_that_only_reported_done(
    rex, workmux_exe_path, repo_path
):
    block = start_fake_agent(rex, workmux_exe_path, repo_path, "osc-last-done")
    report(rex, block, "state=done:app=fake")
    pruning = workmux(rex, workmux_exe_path, repo_path, "list")
    assert pruning.returncode == 0, pruning.stderr

    assert_last_done_focuses(rex, workmux_exe_path, repo_path, block)


def test_last_done_prefers_a_hook_status_newer_than_the_last_report_a_reader_saw(
    rex, workmux_exe_path, repo_path
):
    block = start_fake_agent(
        rex, workmux_exe_path, repo_path, "osc-hook-newer", launch="{agent}"
    )

    def hook(status):
        result = workmux(
            rex, workmux_exe_path, repo_path, "set-window-status", status, block=block
        )
        assert result.returncode == 0, result.stderr

    hook("working")
    report(rex, block, "state=working:app=fake")
    pruning = workmux(rex, workmux_exe_path, repo_path, "list")
    assert pruning.returncode == 0, pruning.stderr

    # Statuses carry whole seconds, so the hook must land in a later second.
    time.sleep(1.2)
    report(rex, block, "state=done:app=fake")
    hook("done")

    assert_last_done_focuses(rex, workmux_exe_path, repo_path, block)


def hook_status(env: RexEnvironment, exe: Path, repo: Path, block: str, status: str):
    result = workmux(env, exe, repo, "set-window-status", status, block=block)
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize("hook", ["waiting", "done"])
def test_last_done_agrees_with_status_when_a_live_report_outranks_a_newer_hook(
    rex, workmux_exe_path, repo_path, hook
):
    block = start_fake_agent(
        rex, workmux_exe_path, repo_path, f"osc-outranks-{hook}", launch="{agent}"
    )
    report(rex, block, "state=working:app=fake")
    assert workmux(rex, workmux_exe_path, repo_path, "list").returncode == 0

    time.sleep(1.2)
    hook_status(rex, workmux_exe_path, repo_path, block, hook)
    assert workmux(rex, workmux_exe_path, repo_path, "list").returncode == 0

    assert agent_status(rex, workmux_exe_path, repo_path, block) == "working"
    result = workmux(rex, workmux_exe_path, repo_path, "last-done")
    assert result.returncode == 0, result.stderr
    assert "No completed or waiting agents found" in result.stdout + result.stderr


def test_last_done_agrees_with_status_after_a_hook_change_in_the_reports_second(
    rex, workmux_exe_path, repo_path
):
    block = start_fake_agent(
        rex, workmux_exe_path, repo_path, "osc-same-second", launch="{agent}"
    )
    hook_status(rex, workmux_exe_path, repo_path, block, "waiting")
    report(rex, block, "state=working:app=fake")
    assert workmux(rex, workmux_exe_path, repo_path, "list").returncode == 0

    time.sleep(1.2 - time.time() % 1)
    report(rex, block, "state=done:app=fake")
    assert workmux(rex, workmux_exe_path, repo_path, "list").returncode == 0
    hook_status(rex, workmux_exe_path, repo_path, block, "working")
    assert workmux(rex, workmux_exe_path, repo_path, "list").returncode == 0

    assert agent_status(rex, workmux_exe_path, repo_path, block) == "done"
    assert_last_done_focuses(rex, workmux_exe_path, repo_path, block)


UPDATED_AGY = "agy.1793778097211245000.old"


def start_native_agy(env: RexEnvironment, exe: Path, repo: Path, branch: str):
    """Start a native fake `agy` in a new worktree window. A self-updating
    agent renames its running binary to agy.<n>.old, and Rex then names the
    process after the renamed file."""
    cc = shutil.which("cc")
    if not cc:
        pytest.skip("needs a C compiler to build a native fake agent")
    agent = env.tmp_path / "agent-bin" / "agy"
    agent.parent.mkdir()
    source = env.tmp_path / "agy.c"
    source.write_text("#include <unistd.h>\nint main(void) { for (;;) pause(); }\n")
    subprocess.run([cc, "-o", str(agent), str(source)], check=True)
    write_workmux_config(repo, panes=[{"focus": True}])
    run_workmux_add(env, exe, repo, branch)
    window = get_window_name(branch)
    wait_for_window_ready(env, window)
    block = env._target_block(window)
    env.send_keys(block, f"exec {agent}")
    assert poll_until(lambda: foreground_name(env, block) == "agy", timeout=10)
    return block, agent


def agy_hooks(env: RexEnvironment, exe: Path, block: str):
    # agy runs its hooks from its config directory, not the agent's.
    hooks_dir = env.tmp_path / "agy-config"
    hooks_dir.mkdir(exist_ok=True)
    for args in (["register-agent"], ["set-window-status", "working"]):
        hook = workmux(env, exe, hooks_dir, *args, block=block)
        assert hook.returncode == 0, hook.stderr


def update_agy(env: RexEnvironment, agent: Path, block: str):
    agent.rename(agent.with_name(UPDATED_AGY))
    assert poll_until(lambda: foreground_name(env, block) == UPDATED_AGY, timeout=5)


def test_a_hooked_agent_stays_listed_after_renaming_its_running_binary(
    rex, workmux_exe_path, repo_path
):
    branch = "renamed-agent"
    block, agent = start_native_agy(rex, workmux_exe_path, repo_path, branch)
    agy_hooks(rex, workmux_exe_path, block)
    assert agent_status(rex, workmux_exe_path, repo_path, block) == "working"

    update_agy(rex, agent, block)

    result = workmux(rex, workmux_exe_path, repo_path, "status", "--json")
    assert result.returncode == 0, result.stderr
    agents = [a for a in json.loads(result.stdout)["agents"] if a["pane_id"] == block]
    assert len(agents) == 1, result.stdout
    assert agents[0]["status"] == "working"
    assert agents[0]["agent_kind"] == "agy"
    assert agents[0]["worktree"] == branch
    assert (
        Path(agents[0]["workdir"]).resolve()
        == get_worktree_path(repo_path, branch).resolve()
    )


def test_a_reporting_pane_in_the_main_worktree_is_not_adopted(
    rex, workmux_exe_path, repo_path
):
    block = start_fake_agent(
        rex,
        workmux_exe_path,
        repo_path,
        "osc-main-worktree",
        launch=f"cd {repo_path} && exec {{agent}}",
    )
    report(rex, block, "state=working:app=fake")
    assert agent_status(rex, workmux_exe_path, repo_path, block) is None


# The sidebar draws a working agent as a spinner, not an icon.
WORKING = "spinner"


def sidebar_blocks(env) -> list[str]:
    return [
        block["block_id"]
        for view in env.views()
        for window in view.get("windows", [])
        for layer in window.get("layers", [])
        for block in layer.get("blocks", [])
        if block.get("label") == SIDEBAR_LABEL
    ]


def test_the_sidebar_follows_each_status_the_agent_reports(
    rex, workmux_exe_path, repo_path
):
    env = rex
    short_tmp = env.socket_dir / "t"
    short_tmp.mkdir()
    env.env["TMPDIR"] = str(short_tmp)
    block = start_fake_agent(env, workmux_exe_path, repo_path, "side-osc")
    report(env, block, "state=working:app=fake")

    result = workmux(env, workmux_exe_path, repo_path, "sidebar", "on")
    assert result.returncode == 0, result.stderr
    try:
        assert poll_until(lambda: bool(sidebar_blocks(env)), timeout=10), env.views()
        sidebar = sidebar_blocks(env)[0]

        def shows(icon: str) -> bool:
            text = env.capture_pane(sidebar) or ""
            return bool(SPINNER.search(text)) if icon == WORKING else icon in text

        assert poll_until(lambda: shows(WORKING), timeout=10), env.capture_pane(sidebar)
        # Each report is a program_status event the daemon reads again on.
        for body, icon in [
            ("state=blocked:kind=permission:app=fake", "💬"),
            ("state=done:app=fake", "✅"),
            ("state=working:app=fake", WORKING),
        ]:
            report(env, block, body)
            assert poll_until(lambda: shows(icon), timeout=10), (
                body,
                env.capture_pane(sidebar),
            )
    finally:
        workmux(env, workmux_exe_path, repo_path, "sidebar", "off")


def test_the_sidebar_shows_an_agent_that_registers_after_renaming_its_binary(
    rex, workmux_exe_path, repo_path
):
    env = rex
    short_tmp = env.socket_dir / "t"
    short_tmp.mkdir()
    env.env["TMPDIR"] = str(short_tmp)
    block, agent = start_native_agy(env, workmux_exe_path, repo_path, "side-agy")

    result = workmux(env, workmux_exe_path, repo_path, "sidebar", "on")
    assert result.returncode == 0, result.stderr
    try:
        assert poll_until(lambda: bool(sidebar_blocks(env)), timeout=10), env.views()
        sidebar = sidebar_blocks(env)[0]
        assert poll_until(
            lambda: "No agents running" in (env.capture_pane(sidebar) or ""),
            timeout=10,
        ), env.capture_pane(sidebar)

        # The daemon read the pane before the update, and a rename sends no
        # process event, so it keeps the name it read then.
        update_agy(env, agent, block)
        agy_hooks(env, workmux_exe_path, block)
        assert poll_until(
            lambda: bool(SPINNER.search(env.capture_pane(sidebar) or "")), timeout=10
        ), env.capture_pane(sidebar)
    finally:
        workmux(env, workmux_exe_path, repo_path, "sidebar", "off")
