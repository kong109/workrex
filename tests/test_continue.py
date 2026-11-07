"""Tests for resuming agents at recreated worktree paths."""

import json
import shlex
import sys

import pytest

from .conftest import (
    get_worktree_path,
    poll_until,
    run_workrex_command,
    write_workrex_config,
)


# Per agent: arguments before the configured ones, the continue arguments,
# and the arguments before the prompt.
AGENT_LAUNCHES = [
    ("claude", [], ["--continue"], ["--"]),
    # Codex runs without its shared server, so hooks see this pane's environment.
    ("codex", ["--no-daemon"], ["resume", "--last"], ["--"]),
    ("pi", [], ["--continue"], []),
    ("agy", [], ["--continue"], ["-i"]),
    ("grok", [], ["--continue"], []),
    # Kiro's interactive mode is its `chat` subcommand.
    ("kiro-cli", ["chat"], ["--resume"], []),
    ("vibe", [], ["--continue"], []),
]


@pytest.mark.parametrize(
    ("agent", "launch_args", "resume_args", "prompt_args"),
    AGENT_LAUNCHES,
    ids=[launch[0] for launch in AGENT_LAUNCHES],
)
@pytest.mark.parametrize("prompt_flag", [None, "-p", "-P"])
def test_continue_at_recreated_path(
    mux_server,
    workrex_exe_path,
    mux_repo_path,
    fake_agent_installer,
    tmp_path,
    agent,
    launch_args,
    resume_args,
    prompt_args,
    prompt_flag,
):
    """Continuation and optional prompts reach the agent at the original path."""
    output = tmp_path / "launch.json"
    shim = fake_agent_installer.install(
        agent,
        f"#!{sys.executable}\n"
        "import json, os, sys\n"
        f"with open({str(output)!r}, 'w') as f:\n"
        "    json.dump({'cwd': os.getcwd(), 'args': sys.argv[1:]}, f)\n",
    )
    write_workrex_config(
        mux_repo_path, panes=[{"command": "<agent>"}], agent=f"{shim} --model test"
    )
    run_workrex_command(
        mux_server, workrex_exe_path, mux_repo_path, "add original --name reused -b -C"
    )
    original_path = get_worktree_path(mux_repo_path, "reused")
    assert original_path.is_dir()
    run_workrex_command(
        mux_server, workrex_exe_path, mux_repo_path, "remove reused --force"
    )
    assert not original_path.exists()

    prompt = "Follow up on the previous change"
    command = "add followup --name reused --continue -b"
    if prompt_flag == "-p":
        command += f" -p {shlex.quote(prompt)}"
    elif prompt_flag == "-P":
        prompt_file = tmp_path / "followup.md"
        prompt_file.write_text(prompt)
        command += f" -P {shlex.quote(str(prompt_file))}"

    run_workrex_command(mux_server, workrex_exe_path, mux_repo_path, command)
    assert poll_until(lambda: output.exists() and output.stat().st_size > 0)
    launch = json.loads(output.read_text())
    assert launch["cwd"] == str(original_path.resolve())
    expected = [*launch_args, "--model", "test", *resume_args]
    if prompt_flag:
        expected += [*prompt_args, prompt]
    assert launch["args"] == expected
