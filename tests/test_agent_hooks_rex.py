"""Each agent's hooks, as `workrex setup` installs them, driving status.

A test installs the agent's hooks with `workrex setup` into the test HOME,
starts a fake agent under the agent's executable name in a worktree window,
then fires the installed commands with the payload that agent sends.
"""

import json
import os
import re
import signal
from pathlib import Path

import pytest
import yaml

from .conftest import (
    AGENT_VARIABLES,
    get_window_name,
    get_worktree_path,
    poll_until,
    run_workrex_add,
    wait_for_window_ready,
    write_workrex_config,
)
from .rex_env import RexEnvironment
from .support.agent_hooks import (
    HookedAgent,
    agy_format,
    build_native_agent,
    claude_format,
    copilot_format,
    drop_registration,
    fire,
    runs_for,
)
from .support.agent_state import list_agent_state_files, read_agent_state
from .support.setup import install_status_hooks, update_status_hooks
from .test_status_rex import agent_status, foreground_name, window_label, workrex

SESSION = "01a1a592-c141-7850-a3bc-4ce8f1634b27"

MUSE = HookedAgent(
    kind="muse",
    # Muse's launcher execs a versioned binary.
    process="muse-bin-1.4.14-R7309.1",
    config_dir=".config/muse",
    hooks_file=".config/muse/workrex-status.json",
    read=claude_format,
    register="SessionStart",
    events={"working": "UserPromptSubmit", "waiting": "Notification", "done": "Stop"},
    env={"MUSE_SESSION_ID": SESSION},
    payloads={
        "SessionStart": {"source": "startup"},
        **{
            event: {"tool_name": "Bash", "tool_use_id": "toolu_01"}
            for event in ["ToolUseStart", "PostToolUse", "PostToolUseFailure"]
        },
    },
    tool_events=["ToolUseStart", "PostToolUse", "PostToolUseFailure"],
    ending_events=["Interrupt", "SessionEnd"],
)
GROK = HookedAgent(
    kind="grok",
    process="grok",
    config_dir=".grok",
    hooks_file=".grok/hooks/workrex-status.json",
    read=claude_format,
    register="SessionStart",
    events={"working": "UserPromptSubmit", "waiting": "Notification", "done": "Stop"},
    payloads={
        "SessionStart": {"source": "startup"},
        "PostToolUse": {"tool_name": "run_terminal_cmd"},
        "SessionEnd": {"reason": "exit"},
    },
    tool_events=["PostToolUse"],
    ending_events=["SessionEnd"],
)
COPILOT = HookedAgent(
    kind="copilot",
    process="copilot",
    config_dir=".copilot",
    hooks_file=".copilot/hooks/workrex-status.json",
    read=copilot_format,
    register="sessionStart",
    # Copilot's hooks have no permission event, so it never waits.
    events={"working": "userPromptSubmitted", "done": "agentStop"},
    payloads={
        "sessionStart": {"source": "new"},
        "postToolUse": {"toolName": "bash", "toolArgs": "{}"},
    },
    tool_events=["postToolUse"],
)
AGY = HookedAgent(
    kind="agy",
    process="agy",
    config_dir=".gemini/antigravity-cli",
    hooks_file=".gemini/config/hooks.json",
    read=agy_format,
    # Antigravity registers and reports working from one invocation hook.
    register="PreInvocation",
    events={"working": "PreInvocation", "done": "Stop"},
)
GEMINI = HookedAgent(
    kind="gemini",
    process="gemini",
    config_dir=".gemini",
    hooks_file=".gemini/settings.json",
    read=claude_format,
    register="SessionStart",
    events={"working": "BeforeAgent", "waiting": "Notification", "done": "AfterAgent"},
    payloads={
        "SessionStart": {"source": "startup"},
        "Notification": {
            "notification_type": "ToolPermission",
            "message": "Allow run_shell_command?",
        },
        "AfterTool": {"tool_name": "run_shell_command"},
        "SessionEnd": {"reason": "exit"},
    },
    tool_events=["AfterTool"],
    ending_events=["SessionEnd"],
)
CODEX = HookedAgent(
    kind="codex",
    process="codex",
    config_dir=".codex",
    hooks_file=".codex/hooks.json",
    read=claude_format,
    register="SessionStart",
    events={
        "working": "UserPromptSubmit",
        "waiting": "PermissionRequest",
        "done": "Stop",
    },
    payloads={
        "SessionStart": {"source": "startup"},
        "PermissionRequest": {"tool_name": "Bash"},
        "PostToolUse": {"tool_name": "Bash"},
        "SubagentStart": {"agent_id": "a1", "agent_type": "worker"},
        "SubagentStop": {"agent_id": "a1", "agent_type": "worker"},
    },
    tool_events=["PostToolUse", "SubagentStart", "SubagentStop"],
)
AGENTS = [MUSE, AGY, GROK, COPILOT, GEMINI, CODEX]


@pytest.fixture
def rex(mux_server: RexEnvironment) -> RexEnvironment:
    if not isinstance(mux_server, RexEnvironment):
        pytest.skip("agent hooks report to Rex panes")
    return mux_server


class Running:
    """A fake agent in a worktree window with its hooks installed."""

    def __init__(self, env, exe, repo, agent: HookedAgent, branch: str):
        self.env, self.exe, self.repo, self.agent = env, exe, repo, agent
        (env.home_path / agent.config_dir).mkdir(parents=True, exist_ok=True)
        install_status_hooks(env, exe)
        self.hooks = agent.hooks(env.home_path)
        binary = build_native_agent(env.tmp_path / "agent-bin", agent.process)
        write_workrex_config(repo, panes=[{"focus": True}])
        run_workrex_add(env, exe, repo, branch)
        self.window = get_window_name(branch)
        wait_for_window_ready(env, self.window)
        self.block = env._target_block(self.window)
        self.worktree = get_worktree_path(repo, branch)
        env.send_keys(self.block, f"exec {binary}")
        assert poll_until(
            lambda: foreground_name(env, self.block) == agent.process, timeout=10
        ), foreground_name(env, self.block)

    def fire(self, event: str, **fields):
        """Fire `event` with the agent's payload for it, plus `fields`, through
        every installed hook whose matcher the payload matches."""
        payload = {
            "session_id": SESSION,
            "hook_event_name": event,
            **self.agent.payloads.get(event, {}),
            **fields,
        }
        hooks = self.hooks.get(event)
        assert hooks, f"setup installed no {event} hook: {self.hooks}"
        runs = [hook for hook in hooks if runs_for(hook, event, payload)]
        assert runs, f"no installed {event} hook matches {payload}: {hooks}"
        for hook in runs:
            fire(
                self.env,
                self.exe,
                hook.command,
                payload,
                block=self.block,
                cwd=self.worktree,
                extra_env=self.agent.env,
            )

    def status(self) -> str | None:
        return agent_status(self.env, self.exe, self.repo, self.block)

    def label(self) -> str:
        return window_label(self.env, self.block)

    def prompt(self) -> str | None:
        [state] = [
            state
            for state in map(read_agent_state, list_agent_state_files(self.env))
            if state["pane_key"]["pane_id"] == self.block
        ]
        return state.get("prompt")

    def agent_record(self) -> dict:
        result = workrex(self.env, self.exe, self.repo, "status", "--json")
        assert result.returncode == 0, result.stderr
        [record] = [
            a for a in json.loads(result.stdout)["agents"] if a["pane_id"] == self.block
        ]
        return record


ICONS = {"waiting": "💬", "done": "✅"}
# Rex window labels separate a status icon with an en space.
SEPARATOR = "\u2002"


def show_prompts(env: RexEnvironment) -> None:
    """Workrex stores prompts only while a column or template shows them."""
    path = env.home_path / ".config/workrex/config.yaml"
    config = yaml.safe_load(path.read_text()) or {}
    config.setdefault("dashboard", {})["agent_columns"] = ["title", "prompt"]
    path.write_text(yaml.safe_dump(config))


@pytest.mark.parametrize("agent", AGENTS, ids=lambda agent: agent.kind)
def test_installed_hooks_drive_the_agents_status(
    rex, workrex_exe_path, repo_path, agent: HookedAgent
):
    running = Running(rex, workrex_exe_path, repo_path, agent, f"hooks-{agent.kind}")

    running.fire(agent.register, source="startup")
    record = running.agent_record()
    assert record["agent_kind"] == agent.kind
    assert Path(record["workdir"]).resolve() == running.worktree.resolve()

    statuses = ["working", "waiting", "working", "done"]
    for status in [s for s in statuses if s in agent.events]:
        event = agent.events[status]
        if status == "working":
            running.fire(event, prompt="fix the bug")
        else:
            running.fire(event)
        assert running.status() == status, (event, running.agent_record())
        if status in ICONS:
            assert running.label() == f"{ICONS[status]}{SEPARATOR}{running.window}"


TOOL_EVENTS = [(agent, event) for agent in AGENTS for event in agent.tool_events]


@pytest.mark.parametrize(
    ("agent", "event"),
    TOOL_EVENTS,
    ids=[f"{agent.kind}-{event}" for agent, event in TOOL_EVENTS],
)
def test_a_tool_call_mid_turn_keeps_the_agent_working(
    rex, workrex_exe_path, repo_path, agent: HookedAgent, event: str
):
    running = Running(rex, workrex_exe_path, repo_path, agent, f"tool-{agent.kind}")
    running.fire(agent.register)
    running.fire(agent.events["working"], prompt="fix the bug")
    running.fire(event)
    assert running.status() == "working"
    if "waiting" in agent.events:
        # A tool call after a permission prompt means the user answered it.
        running.fire(agent.events["waiting"])
        assert running.status() == "waiting"
        running.fire(event)
        assert running.status() == "working"


ENDING_EVENTS = [(agent, event) for agent in AGENTS for event in agent.ending_events]


@pytest.mark.parametrize(
    ("agent", "event"),
    ENDING_EVENTS,
    ids=[f"{agent.kind}-{event}" for agent, event in ENDING_EVENTS],
)
def test_the_end_of_a_turn_or_session_leaves_the_agent_done(
    rex, workrex_exe_path, repo_path, agent: HookedAgent, event: str
):
    running = Running(rex, workrex_exe_path, repo_path, agent, f"end-{agent.kind}")
    running.fire(agent.register)
    running.fire(agent.events["working"], prompt="fix the bug")
    running.fire(event)
    assert running.status() == "done"
    assert running.label() == f"✅{SEPARATOR}{running.window}"


@pytest.fixture
def muse(rex, workrex_exe_path, repo_path) -> Running:
    return Running(rex, workrex_exe_path, repo_path, MUSE, "hooks-muse-turn")


def finished_turn(muse: Running) -> None:
    muse.fire("SessionStart", source="startup")
    muse.fire("UserPromptSubmit", prompt="fix the bug")
    muse.fire("Stop")
    assert muse.status() == "done"


def test_a_muse_shell_command_after_the_turn_keeps_it_done(muse):
    # `!cmd` runs as a tool call of no turn, with a `user_shell_` id.
    finished_turn(muse)
    for event in ["ToolUseStart", "PostToolUse", "PostToolUseFailure"]:
        muse.fire(event, tool_use_id="user_shell_01a1a5", tool_name="Bash")
        assert muse.status() == "done", event
    assert muse.label() == f"✅{SEPARATOR}{muse.window}"


def test_a_muse_tool_start_after_stop_keeps_the_turn_done(muse):
    # Muse runs ToolUseStart only asynchronously, so it can land after Stop.
    finished_turn(muse)
    muse.fire("ToolUseStart", tool_use_id="toolu_01", tool_name="Bash")
    assert muse.status() == "done"


def test_a_muse_tool_start_clears_waiting_once_the_user_answers(muse):
    muse.fire("SessionStart", source="startup")
    muse.fire("UserPromptSubmit", prompt="fix the bug")
    muse.fire("Notification", message="Muse needs your permission to use Bash")
    assert muse.status() == "waiting"
    muse.fire("ToolUseStart", tool_use_id="toolu_01", tool_name="Bash")
    assert muse.status() == "working"


def test_muse_events_after_stop_keep_the_turn_done_but_can_ask(muse):
    finished_turn(muse)
    for event in ["ToolUseStart", "PostToolUse", "PostToolUseFailure"]:
        muse.fire(event, tool_use_id="toolu_02", post_stop=True)
        assert muse.status() == "done", event
    muse.fire("Notification", post_stop=True, message="Muse needs your input")
    assert muse.status() == "waiting"


@pytest.mark.parametrize(
    ("agent", "delegated"),
    [
        (CODEX, {"agent_id": "a1", "agent_type": "worker"}),
        (GROK, {"subagentType": "explore"}),
    ],
    ids=["codex", "grok"],
)
def test_a_subagents_prompt_does_not_replace_the_users(
    rex, workrex_exe_path, repo_path, agent, delegated
):
    show_prompts(rex)
    running = Running(rex, workrex_exe_path, repo_path, agent, f"sub-{agent.kind}")
    running.fire(agent.register, source="startup")
    running.fire(agent.events["working"], prompt="fix the bug")
    running.fire(agent.events["working"], prompt="search the repo", **delegated)
    assert running.prompt() == "fix the bug"


def test_copilots_session_start_reports_the_initial_prompt(
    rex, workrex_exe_path, repo_path
):
    # Copilot can send sessionStart after the prompt's userPromptSubmitted,
    # so its registration carries the prompt.
    show_prompts(rex)
    running = Running(rex, workrex_exe_path, repo_path, COPILOT, "copilot-prompt")
    running.fire("sessionStart", source="new", initialPrompt="fix the bug")
    assert running.prompt() == "fix the bug"


# `AgentKind::as_str` -> an executable name the agent's pane runs.
AGENT_PROCESSES = {
    "claude": "claude",
    "codex": "codex",
    "opencode": "opencode",
    "gemini": "gemini",
    "agy": "agy",
    "pi": "pi",
    "omp": "omp",
    "kiro-cli": "kiro-cli",
    "vibe": "vibe",
    "copilot": "copilot",
    "grok": "grok",
    "muse": MUSE.process,
}


@pytest.mark.parametrize(
    ("kind", "process"), AGENT_PROCESSES.items(), ids=list(AGENT_PROCESSES)
)
def test_each_agent_is_listed_by_its_process_until_it_exits(
    rex, workrex_exe_path, repo_path, kind, process
):
    env = rex
    binary = build_native_agent(env.tmp_path / "agent-bin", process)
    write_workrex_config(repo_path, panes=[{"focus": True}])
    branch = f"kind-{kind}"
    run_workrex_add(env, workrex_exe_path, repo_path, branch)
    window = get_window_name(branch)
    wait_for_window_ready(env, window)
    block = env._target_block(window)
    shell = foreground_name(env, block)
    env.send_keys(block, str(binary))
    assert poll_until(lambda: foreground_name(env, block) == process, timeout=10)

    hook = workrex(
        env, workrex_exe_path, repo_path, "set-window-status", "working", block=block
    )
    assert hook.returncode == 0, hook.stderr
    result = workrex(env, workrex_exe_path, repo_path, "status", "--json")
    [record] = [a for a in json.loads(result.stdout)["agents"] if a["pane_id"] == block]
    assert (record["agent_kind"], record["status"]) == (kind, "working")

    process_info = env.block_call(block, "process") or {}
    os.kill(process_info["foreground"]["pid"], signal.SIGTERM)
    assert poll_until(lambda: foreground_name(env, block) == shell, timeout=5)
    assert agent_status(env, workrex_exe_path, repo_path, block) is None


@pytest.mark.parametrize("agent", AGENTS, ids=lambda agent: agent.kind)
def test_setup_updates_hooks_an_older_build_installed(
    rex, workrex_exe_path, agent: HookedAgent
):
    (rex.home_path / agent.config_dir).mkdir(parents=True, exist_ok=True)
    install_status_hooks(rex, workrex_exe_path)
    hooks_file = rex.home_path / agent.hooks_file
    drop_registration(hooks_file)

    update_status_hooks(rex, workrex_exe_path, "update available", "register-agent")
    hooks = agent.hooks(rex.home_path)
    assert any("register-agent" in h.command for h in hooks[agent.register]), hooks


AGENT_DIR = re.compile(r"[A-Z][A-Z0-9_]*_(?:HOME|DIR|CONFIG)")
# Directories that aren't an agent's: the test environment points these
# into HOME or doesn't use them.
NOT_AGENT_DIRS = {
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
}
NOT_AGENT_DIRS |= {"XDG_RUNTIME_DIR", "REX_BIN_DIR", "WORKREX_FROZEN_CONFIG"}


def test_the_test_environment_drops_every_agent_directory_variable():
    # A variable the product reads to find a directory, as `env::var("X")`,
    # `get_env("X")`, or a `const` holding the name.
    read = re.compile(
        r'(?:var(?:_os)?|get_env)\(\s*"([A-Z0-9_]+)"\s*\)|const [A-Z0-9_]+: &str = "([A-Z0-9_]+)"'
    )
    source = Path(__file__).parent.parent / "src"
    names = {
        name
        for path in source.rglob("*.rs")
        for match in read.finditer(path.read_text())
        for name in match.groups()
        if name and AGENT_DIR.fullmatch(name)
    }
    assert names - NOT_AGENT_DIRS - set(AGENT_VARIABLES) == set()


def test_setup_in_a_test_writes_only_under_its_home(
    tmp_path, monkeypatch, workrex_exe_path
):
    outside = tmp_path / "outside"
    for name in AGENT_VARIABLES:
        (outside / name).mkdir(parents=True)
        monkeypatch.setenv(name, str(outside / name))
    (tmp_path / "env").mkdir()
    env = RexEnvironment(tmp_path / "env")
    env.start_server()
    try:
        for agent in AGENTS:
            (env.home_path / agent.config_dir).mkdir(parents=True, exist_ok=True)
        install_status_hooks(env, workrex_exe_path)
    finally:
        env.stop_server()
    leaked = [p.relative_to(outside) for p in outside.rglob("*") if p.is_file()]
    assert sorted(map(str, leaked)) == []
    for agent in AGENTS:
        assert (env.home_path / agent.hooks_file).exists(), agent.kind
