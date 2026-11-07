"""Workrex cleanup runs from real Rex session blocks and retains stable identity."""

import shlex
from pathlib import Path

import pytest

from .conftest import (
    WorkrexCommandResult,
    get_scripts_dir,
    get_session_name,
    poll_until,
    poll_until_file_has_content,
    run_workrex_command,
    write_workrex_config,
)
from .rex_env import RexEnvironment
from .test_workrex_add.conftest import add_branch_and_get_worktree


def run_workrex_in_session(
    env: RexEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    session_name: str,
    command: str,
    working_dir: Path | None = None,
    rust_log: str | None = None,
) -> WorkrexCommandResult:
    """Run a workrex command from inside a specific Rex session.

    Unlike run_workrex_command which runs from the test session, this sends
    the command to a pane inside the target session. This allows testing
    behavior that depends on being inside the session (e.g.,
    is_inside_matching_target detection in cleanup.rs).

    The command is executed via a script file to avoid terminal input line
    length limits. Exit code, stdout, and stderr are captured to files and
    returned after the command completes.

    When rust_log is set, XDG_STATE_HOME is redirected to a temp directory
    so the log file can be read back. workrex writes tracing output to a
    log file (not stderr), so the log content is appended to stderr in the
    returned result.

    The script ignores the pane's hangup signal so it can capture the command's
    exit code after a deferred cleanup kills the session.
    """
    scripts_dir = get_scripts_dir(env)
    stdout_file = scripts_dir / "in_session_stdout.txt"
    stderr_file = scripts_dir / "in_session_stderr.txt"
    exit_code_file = scripts_dir / "in_session_exit.txt"
    script_file = scripts_dir / "in_session_run.sh"
    state_dir = scripts_dir / "xdg_state"
    state_dir.mkdir(exist_ok=True)

    for f in [stdout_file, stderr_file, exit_code_file]:
        if f.exists():
            f.unlink()

    # Clear any previous log file so we only see output from this run
    log_file = state_dir / "workrex" / "workrex.log"
    if log_file.exists():
        log_file.unlink()

    workdir = working_dir or repo_path

    rust_log_line = ""
    if rust_log:
        rust_log_line = f"export RUST_LOG={shlex.quote(rust_log)}"

    script_content = f"""#!/bin/sh
export PATH={shlex.quote(env.env["PATH"])}
export TMPDIR={shlex.quote(env.env.get("TMPDIR", "/tmp"))}
export HOME={shlex.quote(env.env.get("HOME", ""))}
export XDG_STATE_HOME={shlex.quote(str(state_dir))}
export WORKREX_TEST=1
{rust_log_line}
cd {shlex.quote(str(workdir))}
{shlex.quote(str(workrex_exe_path))} {command} > {shlex.quote(str(stdout_file))} 2> {shlex.quote(str(stderr_file))}
status=$?
echo "$status" > {shlex.quote(str(exit_code_file))}
exit "$status"
"""
    script_file.write_text(script_content)

    # Ignore the pane hangup so deferred session removal cannot interrupt capture.
    env.send_keys(
        f"={session_name}:",
        f"nohup /bin/sh {shlex.quote(str(script_file))} >/dev/null 2>&1 &",
    )

    # Wait for completion (longer timeout to account for deferred scripts)
    if not poll_until_file_has_content(exit_code_file, timeout=10.0):
        pane_content = env.capture_pane(f"={session_name}:") or "(session closed)"
        raise AssertionError(
            f"Command did not complete in time (session: {session_name})\n"
            f"Pane content:\n{pane_content}"
        )

    stderr_content = stderr_file.read_text() if stderr_file.exists() else ""

    # Append workrex log file content to stderr when RUST_LOG was set.
    # workrex writes tracing output to $XDG_STATE_HOME/workrex/workrex.log
    # (not stderr), so we read it back and include it in the result.
    if rust_log and log_file.exists():
        stderr_content += log_file.read_text()

    return WorkrexCommandResult(
        exit_code=int(exit_code_file.read_text().strip()),
        stdout=stdout_file.read_text() if stdout_file.exists() else "",
        stderr=stderr_content,
    )


@pytest.mark.parametrize("operation", ["close", "remove"])
def test_cleanup_from_inside_rex_session(
    mux_server: RexEnvironment, workrex_exe_path: Path, repo_path: Path, operation: str
):
    env = mux_server
    branch = f"inside-{operation}"
    write_workrex_config(repo_path, panes=[{"command": "/bin/sh"}])
    worktree = add_branch_and_get_worktree(
        env,
        workrex_exe_path,
        repo_path,
        branch,
        extra_args="--mode session --background",
    )
    label = get_session_name(branch)
    session_id = env.session_view(label)["session_id"]
    result = run_workrex_in_session(
        env,
        workrex_exe_path,
        worktree,
        label,
        "remove --force" if operation == "remove" else "close",
    )
    assert result.exit_code == 0, result.stderr
    assert poll_until(
        lambda: env.session_result(session_id).returncode != 0, timeout=10
    )
    if operation == "remove":
        assert poll_until(lambda: not worktree.exists(), timeout=10)
    else:
        assert worktree.exists()
    assert env.session_result(env.session_id).returncode == 0


def test_renamed_session_cleanup_uses_recorded_identity(
    mux_server: RexEnvironment, workrex_exe_path: Path, repo_path: Path
):
    env = mux_server
    branch = "renamed-session"
    write_workrex_config(repo_path)
    worktree = add_branch_and_get_worktree(
        env,
        workrex_exe_path,
        repo_path,
        branch,
        extra_args="--mode session --background",
    )
    session_id = env.session_view(get_session_name(branch))["session_id"]
    env.rename_session(session_id, "renamed-by-user")
    env.new_session(get_session_name(branch))
    replacement = env.session_view(get_session_name(branch))["session_id"]
    run_workrex_command(env, workrex_exe_path, repo_path, f"close {branch}")
    assert poll_until(lambda: env.session_result(session_id).returncode != 0)
    assert env.session_result(replacement).returncode == 0
    assert worktree.exists()
