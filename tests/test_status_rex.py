"""Agent status from Rex program status (OSC 7501), falling back to hooks."""

import json
import subprocess
import textwrap
from pathlib import Path
from typing import Optional

import pytest

from .conftest import (
    MuxEnvironment,
    get_window_name,
    poll_until,
    run_workmux_add,
    wait_for_window_ready,
    write_workmux_config,
)
from .rex_env import RexEnvironment

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


def agent_status(env: RexEnvironment, exe: Path, repo: Path, block: str) -> Optional[str]:
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


def test_program_status_wins_over_a_stale_hook_status(
    rex, workmux_exe_path, repo_path
):
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
