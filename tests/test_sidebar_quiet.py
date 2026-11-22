"""Quiet presentation through the real Rex daemon and sidebar.

Run with `--backend=rex`.
"""

import subprocess

import pytest

from .conftest import poll_until
from .test_sidebar_rex import (
    capture,
    cli,
    placed_sidebar,
    rex,  # noqa: F401
    set_status,
    start_agent,
    windows,
)


@pytest.mark.parametrize("nerdfont", [False, True])
def test_quiet_sidebar_hides_working_timer(rex, nerdfont: bool):  # noqa: F811
    env = rex
    config = env.home_path / ".config/workrex/config.yaml"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text(
        f"nerdfont: {str(nerdfont).lower()}\n"
        "stale_after: 1h\n"
        "sidebar:\n"
        "  git_status: false\n"
        "  dim_stale: false\n"
        "  layout: compact\n"
        "  templates:\n"
        '    compact: "{status_icon} {status_label} timer={elapsed}"\n'
    )
    start_agent(env)
    cli(env, "on", "--width", "40")
    sidebar = placed_sidebar(next(iter(windows(env, env.session_id).values())))

    def screen() -> str:
        return capture(env, sidebar["block_id"])

    set_status(env, "working")
    assert poll_until(lambda: "Working timer=0:" in screen(), timeout=10), screen()
    assert poll_until(lambda: "Quiet" in screen(), timeout=25), screen()
    quiet_row = next(line.strip() for line in screen().splitlines() if "Quiet" in line)
    assert quiet_row == "○ Quiet timer="

    # Quiet is presentation only, not successful completion or status clearing.
    state = subprocess.run(
        [str(env.workrex), "status", "--json", "--all"],
        env=env.env,
        cwd=env.tmp_path,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    assert '"status": "working"' in state, state
    set_status(env, "working")
    assert poll_until(
        lambda: "Working" in screen() and "Quiet" not in screen(), timeout=10
    ), screen()
