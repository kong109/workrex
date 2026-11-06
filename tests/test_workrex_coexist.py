"""Workrex setup and removal preserve another tool's hooks and files."""

import json
import shlex
from pathlib import Path

from .conftest import get_scripts_dir, make_env_script, poll_until_file_has_content
from .rex_env import RexEnvironment
from .support.bin_name import BIN
from .support.setup import install_opencode_version, run_setup_with_answers

REPO = Path(__file__).parent.parent


def run_uninstall(env: RexEnvironment, exe: Path, exit_code: int = 0) -> str:
    scripts_dir = get_scripts_dir(env)
    output = scripts_dir / "uninstall_output.txt"
    done = scripts_dir / "uninstall_exit_code.txt"
    done.unlink(missing_ok=True)
    script = make_env_script(
        env,
        f"{shlex.quote(str(exe))} uninstall > {shlex.quote(str(output))} 2>&1; "
        f"echo $? > {shlex.quote(str(done))}",
        {
            "PATH": env.env["PATH"],
            "HOME": env.env.get("HOME", ""),
            "TMPDIR": env.env.get("TMPDIR", "/tmp"),
            "XDG_CONFIG_HOME": env.env.get("XDG_CONFIG_HOME", ""),
            "XDG_STATE_HOME": env.env.get("XDG_STATE_HOME", ""),
            "XDG_CACHE_HOME": str(env.home_path / ".cache"),
        },
    )
    env.send_keys("test:", script, enter=True)
    assert poll_until_file_has_content(done, timeout=10)
    assert done.read_text().strip() == str(exit_code), output.read_text()
    return output.read_text()


def foreign_hooks(home: Path) -> dict[Path, bytes]:
    plugin = json.loads(
        (REPO / ".claude-plugin/plugin.json").read_text().replace(BIN, "other-tool")
    )
    codex = (
        (REPO / "resources/codex/hooks/workrex-status.json")
        .read_text()
        .replace(BIN, "other-tool")
    )
    files = {
        home / ".claude/settings.json": json.dumps(
            {"theme": "dark", "hooks": plugin["hooks"]}
        ),
        home / ".codex/hooks.json": codex,
    }
    for agent in ["pi", "omp"]:
        files[home / f".{agent}/agent/extensions/other-tool-status.ts"] = (
            (REPO / f"resources/{agent}/extensions/workrex-status.ts")
            .read_text()
            .replace(BIN, "other-tool")
        )
    files[home / ".config/opencode/plugins/other-tool-status.ts"] = (
        (REPO / "resources/opencode/plugins/workrex-status.ts")
        .read_text()
        .replace(BIN, "other-tool")
    )
    files[home / ".grok/hooks/other-tool-status.json"] = (
        (REPO / ".grok/hooks/workrex-status.json")
        .read_text()
        .replace(BIN, "other-tool")
    )
    files[home / ".copilot/hooks/other-tool-status.json"] = (
        (REPO / "resources/copilot/hooks/workrex-status/hooks.json")
        .read_text()
        .replace(BIN, "other-tool")
    )
    files[home / ".agents/skills/user-flow/SKILL.md"] = (
        "---\nname: user-flow\ndescription: User flow\n---\nUser instructions\n"
    )
    files[home / ".local/state/other-tool/agents/user.json"] = "{}"
    for path, content in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    return {path: path.read_bytes() for path in files}


def test_setup_and_uninstall_preserve_foreign_hooks_and_state(
    mux_server: RexEnvironment, workrex_exe_path: Path
):
    env = mux_server
    install_opencode_version(env, "v1.18.29")
    before = foreign_hooks(env.home_path)
    run_setup_with_answers(env, workrex_exe_path, skills_answer="y")
    for path in [
        env.home_path / ".claude/settings.json",
        env.home_path / ".codex/hooks.json",
    ]:
        commands = [
            hook["command"]
            for groups in json.loads(path.read_text())["hooks"].values()
            for group in groups
            for hook in group["hooks"]
        ]
        assert any(command.startswith("other-tool ") for command in commands)
        assert any(command.startswith(f"{BIN} ") for command in commands)
    run_uninstall(env, workrex_exe_path)
    for path, content in before.items():
        if path.name in ["settings.json", "hooks.json"]:
            assert json.loads(path.read_bytes()) == json.loads(content), path
        else:
            assert path.read_bytes() == content, path
    assert not (env.home_path / ".agents/skills/workrex/SKILL.md").exists()
