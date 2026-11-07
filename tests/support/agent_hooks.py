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
    """Picks the occasions of its event the hook runs on, by the agent's
    `MatcherRules`; none runs always."""


Hooks = dict[str, list[Hook]]

Test = Callable[[str, str], bool]
"""Whether a matcher selects the subject an event's payload names."""

MatcherRules = dict[str, tuple[str, Test]]
"""Event -> the payload field its matcher tests and how. An agent ignores a
matcher on any other event."""


def search(matcher: str, subject: str) -> bool:
    try:
        return re.search(matcher, subject) is not None
    except re.error:
        return False


def claude_code_test(names: str) -> Test:
    """Claude Code's `Hte`: `*` or empty matches all; a matcher of only
    `names` characters lists exact names; any other is a regex that may
    match any part of the subject. Claude's tool-name aliases are left out."""

    def test(matcher: str, subject: str) -> bool:
        if matcher in ("", "*"):
            return True
        if re.fullmatch(f"[{names}]+", matcher):
            listed = [name.strip() for name in re.split("[|,]", matcher)]
            return subject in [name for name in listed if name]
        return search(matcher, subject)

    return test


def codex_test(matcher: str, subject: str) -> bool:
    """codex-rs `HookMatcher`: the same rules with `|` the only separator."""
    if matcher in ("", "*"):
        return True
    if re.fullmatch("[A-Za-z0-9_|]+", matcher):
        return subject in matcher.split("|")
    return search(matcher, subject)


def gemini_tool_test(matcher: str, subject: str) -> bool:
    """Gemini CLI `HookPlanner.matchesToolName`: an unanchored regex, or
    equality when the matcher isn't one."""
    matcher = matcher.strip()
    if matcher in ("", "*"):
        return True
    try:
        return re.search(matcher, subject) is not None
    except re.error:
        return matcher == subject


def gemini_trigger_test(matcher: str, subject: str) -> bool:
    """Gemini CLI `HookPlanner.matchesTrigger`: equality."""
    matcher = matcher.strip()
    return matcher in ("", "*") or matcher == subject


def copilot_test(matcher: str, subject: str) -> bool:
    """GitHub's hooks reference: compiled as `^(?:PATTERN)$`; an invalid
    pattern skips the hook."""
    try:
        return re.fullmatch(f"(?:{matcher})", subject) is not None
    except re.error:
        return False


CLAUDE_CODE_LIST = "A-Za-z0-9_|, -"
CLAUDE_CODE_RULES: MatcherRules = {
    **{
        event: ("tool_name", claude_code_test(CLAUDE_CODE_LIST))
        for event in [
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "PermissionRequest",
            "PermissionDenied",
        ]
    },
    "SessionStart": ("source", claude_code_test(CLAUDE_CODE_LIST)),
    "SessionEnd": ("reason", claude_code_test(CLAUDE_CODE_LIST)),
    "Notification": ("notification_type", claude_code_test(CLAUDE_CODE_LIST)),
    "PreCompact": ("trigger", claude_code_test(CLAUDE_CODE_LIST)),
    "PostCompact": ("trigger", claude_code_test(CLAUDE_CODE_LIST)),
    "SubagentStart": ("agent_type", claude_code_test(CLAUDE_CODE_LIST)),
    "SubagentStop": ("agent_type", claude_code_test(CLAUDE_CODE_LIST)),
    "StopFailure": ("error", claude_code_test("A-Za-z0-9_|")),
}
# Grok's bundled docs: "Claude Code uses the same rules." Grok ignores a
# matcher on Stop and UserPromptSubmit, as Claude Code does.
GROK_RULES: MatcherRules = CLAUDE_CODE_RULES
# Muse reads Claude-format hooks; its own matcher rules aren't published,
# so its hooks are held to Claude Code's, with ToolUseStart as a tool event.
MUSE_RULES: MatcherRules = {
    **CLAUDE_CODE_RULES,
    "ToolUseStart": ("tool_name", claude_code_test(CLAUDE_CODE_LIST)),
}
# codex-rs `matcher_pattern_for_event` drops the matcher on every other event.
CODEX_RULES: MatcherRules = {
    "PreToolUse": ("tool_name", codex_test),
    "PermissionRequest": ("tool_name", codex_test),
    "PostToolUse": ("tool_name", codex_test),
    "SessionStart": ("source", codex_test),
    "SessionEnd": ("reason", codex_test),
    "SubagentStart": ("agent_type", codex_test),
    "SubagentStop": ("agent_type", codex_test),
    "PreCompact": ("trigger", codex_test),
    "PostCompact": ("trigger", codex_test),
}
# Gemini CLI 0.54.4 `HookPlanner.matchesContext`: tool events test the tool
# name, session events their trigger, and every other event, Notification
# among them, runs its hooks whatever the matcher.
GEMINI_RULES: MatcherRules = {
    "BeforeTool": ("tool_name", gemini_tool_test),
    "AfterTool": ("tool_name", gemini_tool_test),
    "SessionStart": ("source", gemini_trigger_test),
    "SessionEnd": ("reason", gemini_trigger_test),
    "PreCompress": ("trigger", gemini_trigger_test),
}
COPILOT_RULES: MatcherRules = {
    "preToolUse": ("toolName", copilot_test),
    "postToolUse": ("toolName", copilot_test),
    "permissionRequest": ("toolName", copilot_test),
    "notification": ("notification_type", copilot_test),
    "preCompact": ("trigger", copilot_test),
    "subagentStart": ("agentName", copilot_test),
}


def runs_for(rules: MatcherRules | None, hook: Hook, event: str, payload: dict) -> bool:
    """Whether an agent following `rules` runs `hook` for `event` with
    `payload`. With no known rules, a matcher can't be judged."""
    if hook.matcher is None:
        return True
    assert rules is not None, f"no known rules judge the matcher of {hook}"
    if event not in rules:
        return True
    field, test = rules[event]
    assert field in payload, f"{event} matchers test {field}, missing from {payload}"
    return test(hook.matcher, str(payload[field]))


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
    """`{"hooks": {event: [{"bash": ..., "matcher": ...}]}}`."""
    return {
        event: [Hook(hook["bash"], hook.get("matcher")) for hook in hooks]
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
    matching: MatcherRules | None
    """How the agent picks an event's hooks by their matchers; None where
    the rules aren't known."""
    register: str = "SessionStart"
    """The event whose hook registers the agent."""
    events: dict[str, str] = field(default_factory=dict)
    """Status name -> the event the agent fires for it."""
    env: dict[str, str] = field(default_factory=dict)
    """Variables the agent sets for the hooks it runs."""

    payloads: dict[str, dict] = field(default_factory=dict)
    """Event -> the fields the agent sends with it."""
    occasions: dict[str, list[dict]] = field(default_factory=dict)
    """Event -> the fields that tell apart the occasions the agent sends it
    on, such as each tool or session source. Each must run a hook."""
    tool_events: list[str] = field(default_factory=list)
    """Events the agent sends for a tool call inside a turn."""
    ending_events: list[str] = field(default_factory=list)
    """Events besides the `done` one that end the turn or the session."""

    def occasions_of(self, event: str) -> list[dict]:
        return self.occasions.get(event, [{}])

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
