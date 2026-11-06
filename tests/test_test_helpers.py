"""Regression tests for command execution and identity in the test harness."""

import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

from .conftest import make_env_script, poll_until, run_workrex_command
from .rex_env import RexEnvironment


def test_env_script_runs_through_interpreter(tmp_path: Path):
    """Generated commands read scripts, without requiring shebang execution."""
    env = RexEnvironment(tmp_path)
    scripts = tmp_path / "scripts with 'quotes'"
    scripts.mkdir()
    env._scripts_dir = scripts
    value = "spaces, 'quotes', and $literal"
    command = make_env_script(
        env, 'printf "%s" "$HARNESS_VALUE"; exit 17', {"HARNESS_VALUE": value}
    )
    script = next(scripts.iterdir())
    script.chmod(0o644)

    result = env.run_command(["/bin/sh", "-c", command], check=False)

    assert result.returncode == 17
    assert result.stdout == value
    assert result.stderr == ""
    assert "HARNESS_VALUE" not in env.env


@pytest.mark.parametrize("exit_code", [0, 17])
def test_workrex_command_reads_script_through_interpreter(
    mux_server, repo_path: Path, monkeypatch: pytest.MonkeyPatch, exit_code: int
):
    """The terminal runner reads its script and preserves command IO and status."""
    env = mux_server
    scripts = env.tmp_path / "scripts with 'quotes'"
    scripts.mkdir()
    env._scripts_dir = scripts
    send_keys = env.send_keys

    def send_without_execute_permission(target, text, enter=True):
        (scripts / "workrex_run.sh").chmod(0o644)
        send_keys(target, text, enter=enter)

    monkeypatch.setattr(env, "send_keys", send_without_execute_permission)
    result = run_workrex_command(
        env,
        Path("/bin/sh"),
        repo_path,
        '-c \'read -r input; printf "%s\\n" "$input" "$HARNESS_VALUE"; '
        f"pwd; printf error >&2; exit {exit_code}'",
        stdin_input="literal input\n",
        pre_run_env={"HARNESS_VALUE": "spaces and $literal"},
        working_dir=scripts,
        expect_fail=exit_code != 0,
    )

    assert result.exit_code == exit_code
    assert result.stdout.splitlines() == [
        "literal input",
        "spaces and $literal",
        str(scripts.resolve()),
    ]
    assert result.stderr == "error"
    assert "HARNESS_VALUE" not in env.env


@pytest.mark.parametrize(
    "interpreter", ["/bin/sh", "/usr/bin/env bash", sys.executable]
)
@pytest.mark.parametrize("absolute", [False, True], ids=["PATH", "absolute"])
@pytest.mark.parametrize("exit_code", [0, 17])
def test_shared_script_preserves_process_contract(
    tmp_path: Path, interpreter: str, exit_code: int, absolute: bool
):
    """A PATH-resolved test double execs its interpreter without a helper child."""
    from .support.executable import install_script

    bin_dir = tmp_path / "bin with 'quotes'"
    bin_dir.mkdir()
    if interpreter == sys.executable:
        body = (
            "import os, sys\n"
            "line = sys.stdin.readline().rstrip('\\n')\n"
            "print(os.getpid(), line, sys.argv[1], os.environ['VALUE'], os.getcwd(), sep='\\n')\n"
            "print('error', end='', file=sys.stderr)\n"
            f"sys.exit({exit_code})\n"
        )
    else:
        body = (
            "read -r input\n"
            'printf "%s\\n" "$$" "$input" "$1" "$VALUE" "$PWD"\n'
            "printf error >&2\n"
            f"exit {exit_code}\n"
        )
    command = install_script(bin_dir / "agent", f"#!{interpreter}\n{body}")
    payload = command.with_name(command.name + ".script")
    assert not os.access(payload, os.X_OK)
    shadow = tmp_path / "non-executable-first"
    shadow.mkdir()
    (shadow / "agent").touch()
    env = {
        **os.environ,
        "PATH": f"{shadow}:{bin_dir}:{os.environ['PATH']}",
        "VALUE": "$literal",
    }
    child = subprocess.Popen(
        [str(command) if absolute else command.name, "spaces, 'quotes', and λ"],
        cwd=tmp_path,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    stdout, stderr = child.communicate("input\n", timeout=5)
    assert child.returncode == exit_code
    assert stdout.splitlines() == [
        str(child.pid),
        "input",
        "spaces, 'quotes', and λ",
        "$literal",
        str(tmp_path.resolve()),
    ]
    assert stderr == "error"


def test_shared_script_reinstallation_and_signals(tmp_path: Path, script_runner: Path):
    from .support.executable import install_script

    command = install_script(tmp_path / "agent", "#!/bin/sh\nexit 17\n")
    assert subprocess.run([str(command)], check=False).returncode == 17
    original_binary = script_runner.read_bytes()
    install_script(command, '#!/bin/sh\nkill -TERM "$$"\n')
    assert subprocess.run([str(command)], check=False).returncode == -signal.SIGTERM
    assert script_runner.read_bytes() == original_binary


def test_rex_environment_does_not_inherit_host_identity(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setenv("REX_SESSION", "/host/socket,12345,9")
    monkeypatch.setenv("REX_BLOCK", "%987654")
    env = RexEnvironment(tmp_path)
    assert "REX_SESSION" not in env.env
    assert "REX_BLOCK" not in env.env


def test_background_command_origin_does_not_follow_active_window(
    mux_server: RexEnvironment, tmp_path: Path
):
    import shlex

    env = mux_server
    runner = env.runner_block_id
    env.new_window("active-worktree")
    env.select_window("active-worktree")
    active = env.read_current("block_id").stdout.strip()
    assert active != runner
    output = tmp_path / "background-origin"
    env.run_shell_background(
        f"printf '%s' \"${{REX_BLOCK-unset}}\" > {shlex.quote(str(output))}"
    )
    assert poll_until(lambda: output.exists() and output.read_text() == runner)
    assert env.read_current("block_id").stdout.strip() == active
