"""Run an agent's installed status hooks the way that agent runs them.

`workrex setup` writes each agent's hooks in the agent's own format. These
helpers read the installed file back, so a test fires the commands setup
actually wrote, with the payload each agent sends on stdin.
"""

import json
import re
import shutil
import subprocess
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

import pytest

from ..rex_env import RexEnvironment


@dataclass(frozen=True)
class Hook:
    command: str
    matcher: str | None = None
    """A regex the event's subject must match in full; none runs always."""


Hooks = dict[str, list[Hook]]

# The payload field an event's matcher tests, in Claude Code's format and
# the agents that follow it.
MATCHED_FIELDS = {
    "SessionStart": "source",
    "SessionEnd": "reason",
    "Notification": "notification_type",
    "PreToolUse": "tool_name",
    "PostToolUse": "tool_name",
    "PostToolUseFailure": "tool_name",
    "PermissionRequest": "tool_name",
    "BeforeTool": "tool_name",
    "AfterTool": "tool_name",
}


def runs_for(hook: Hook, event: str, payload: dict) -> bool:
    """Whether the agent runs `hook` for `event` with `payload`."""
    if hook.matcher in (None, "", "*"):
        return True
    subject = str(payload.get(MATCHED_FIELDS.get(event, ""), ""))
    return re.fullmatch(hook.matcher, subject) is not None


def claude_format(config: dict) -> Hooks:
    """`{"hooks": {Event: [{"matcher": ..., "hooks": [{"command": ...}]}]}}`,
    the format Claude Code set and Muse, Grok, Gemini and Codex follow."""
    return {
        event: [
            Hook(hook["command"], group.get("matcher"))
            for group in groups
            for hook in group["hooks"]
        ]
        for event, groups in config["hooks"].items()
    }


def copilot_format(config: dict) -> Hooks:
    """`{"hooks": {event: [{"bash": ...}]}}`."""
    return {
        event: [Hook(hook["bash"]) for hook in hooks]
        for event, hooks in config["hooks"].items()
    }


def agy_format(config: dict) -> Hooks:
    """`{"workrex-status": {Event: [{"command": ...}]}}`, one named group in
    Antigravity's shared hooks file."""
    return {
        event: [Hook(hook["command"], hook.get("matcher")) for hook in hooks]
        for event, hooks in config["workrex-status"].items()
    }


@dataclass
class HookedAgent:
    kind: str
    """`AgentKind::as_str`, what `status --json` reports as `agent_kind`."""
    process: str
    """The executable name the agent's pane runs, which classifies it."""
    config_dir: str
    """Created under HOME so setup detects the agent."""
    hooks_file: str
    """Where setup installs the hooks, relative to HOME."""
    read: Callable[[dict], Hooks]
    register: str = "SessionStart"
    """The event whose hook registers the agent."""
    events: dict[str, str] = field(default_factory=dict)
    """Status name -> the event the agent fires for it."""
    env: dict[str, str] = field(default_factory=dict)
    """Variables the agent sets for the hooks it runs."""

    payloads: dict[str, dict] = field(default_factory=dict)
    """Event -> the fields the agent sends with it."""
    tool_events: list[str] = field(default_factory=list)
    """Events the agent sends for a tool call inside a turn."""
    ending_events: list[str] = field(default_factory=list)
    """Events besides the `done` one that end the turn or the session."""

    def hooks(self, home: Path) -> Hooks:
        return self.read(json.loads((home / self.hooks_file).read_text()))


NATIVE_AGENT = "#include <unistd.h>\nint main(void) { for (;;) pause(); }\n"


def build_native_agent(directory: Path, name: str) -> Path:
    """A program named `name` that runs until killed. Rex names a pane's
    process after the executable, so a script would show its interpreter."""
    cc = shutil.which("cc")
    if not cc:
        pytest.skip("needs a C compiler to build a native fake agent")
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / "agent.c"
    source.write_text(NATIVE_AGENT)
    agent = directory / name
    subprocess.run([cc, "-o", str(agent), str(source)], check=True)
    return agent


def fire(
    env: RexEnvironment,
    exe: Path,
    command: str,
    payload: dict,
    *,
    block: str,
    cwd: Path,
    extra_env: dict[str, str] | None = None,
) -> None:
    """Run one installed hook `command` in `sh`, with `payload` on stdin and
    the pane's variables, as the agent would."""
    run_env = {**env.env, "REX_BLOCK": block, **(extra_env or {})}
    run_env["PATH"] = f"{exe.parent}:{run_env['PATH']}"
    result = subprocess.run(
        ["sh", "-c", command],
        input=json.dumps(payload),
        cwd=cwd,
        env=run_env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, (command, result.stderr)


def drop_registration(path: Path) -> None:
    """Rewrite an installed hooks file as an older build left it: status
    hooks without the `register-agent` hook."""

    def strip(value):
        if isinstance(value, dict):
            if "register-agent" in str(value.get("command") or value.get("bash") or ""):
                return None
            kept = {k: strip(v) for k, v in value.items()}
            if kept.get("hooks") == []:
                # A matcher group with no hooks left goes with its hooks.
                return None
            return {k: v for k, v in kept.items() if v not in (None, [])}
        if isinstance(value, list):
            return [v for v in map(strip, value) if v is not None]
        return value

    config = strip(json.loads(path.read_text()))
    assert "register-agent" not in json.dumps(config)
    path.write_text(json.dumps(config))
