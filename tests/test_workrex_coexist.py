"""workrex coexists with upstream workmux: setup and uninstall leave upstream's hooks alone."""

import json
import shlex
from pathlib import Path

from .conftest import (
    MuxEnvironment,
    get_scripts_dir,
    make_env_script,
    poll_until_file_has_content,
)
from .support.bin_name import BIN, hook_bin, rebrand
from .support.setup import install_opencode_version, run_setup_with_answers

REPO = Path(__file__).parent.parent

UPSTREAM_STOP = "workmux set-window-status done"


def pretty(value: dict) -> str:
    """JSON as upstream's `write_pretty` writes it, so a rewrite of unchanged entries is a no-op."""
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


def write_upstream_hooks(home: Path) -> dict[Path, bytes]:
    """Write hooks the way upstream `workmux setup` installs them; return each file's bytes."""
    plugin = json.loads((REPO / ".claude-plugin/plugin.json").read_text())
    codex = json.loads((REPO / "resources/codex/hooks/workmux-status.json").read_text())
    files = {
        home / ".claude/settings.json": pretty(
            {
                "enabledPlugins": {"workmux-status@workmux": True},
                "hooks": plugin["hooks"],
                "theme": "dark",
            }
        ),
        home / ".codex/hooks.json": pretty(codex),
        home / ".pi/agent/extensions/workmux-status.ts": (
            REPO / "resources/pi/extensions/workmux-status.ts"
        ).read_text(),
        home / ".copilot/hooks/workmux-status.json": (
            REPO / "resources/copilot/hooks/workmux-status/hooks.json"
        ).read_text(),
        home / ".config/opencode/plugins/workmux-status.ts": (
            REPO / "resources/opencode/plugins/workmux-status.ts"
        ).read_text(),
    }
    for path, text in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return {path: path.read_bytes() for path in files}


def run_uninstall(env: MuxEnvironment, exe: Path) -> str:
    scripts_dir = get_scripts_dir(env)
    output = scripts_dir / "uninstall_output.txt"
    done = scripts_dir / "uninstall_exit_code.txt"
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
        },
    )
    env.send_keys("test:", script, enter=True)
    assert poll_until_file_has_content(done, timeout=10)
    assert done.read_text().strip() == "0", output.read_text()
    return output.read_text()


def hook_commands(settings_path: Path) -> list[str]:
    hooks = json.loads(settings_path.read_text()).get("hooks", {})
    return [
        hook["command"]
        for groups in hooks.values()
        for group in groups
        for hook in group["hooks"]
    ]


def test_setup_then_uninstall_leaves_upstream_hooks_byte_identical(
    mux_server: MuxEnvironment, workmux_exe_path: Path
):
    home = mux_server.home_path
    install_opencode_version(mux_server, "v1.18.29")
    upstream = write_upstream_hooks(home)
    state = home / ".local/state/workmux"
    (state / "agents").mkdir(parents=True)
    (state / "agents/upstream.json").write_text("{}")
    claude = home / ".claude/settings.json"

    run_setup_with_answers(
        mux_server,
        workmux_exe_path,
        expected_output=("Upstream workmux status hooks are also installed for",),
    )

    # Shared JSON files gain workrex's hooks beside upstream's; the rest are untouched.
    shared = [claude, home / ".codex/hooks.json"]
    for path in shared:
        commands = hook_commands(path)
        assert UPSTREAM_STOP in commands, path
        assert rebrand(UPSTREAM_STOP) in commands, path
        assert {c.split()[0] for c in commands} == {"workmux", hook_bin()}, path
    assert json.loads(claude.read_text())["enabledPlugins"] == {
        "workmux-status@workmux": True
    }
    assert (home / f".pi/agent/extensions/{BIN}-status.ts").is_file()
    assert (home / f".copilot/hooks/{BIN}-status.json").is_file()
    for path, before in upstream.items():
        if path not in shared:
            assert path.read_bytes() == before, path

    output = run_uninstall(mux_server, workmux_exe_path)
    assert "Kept the directories workmux shares" in output

    for path, before in upstream.items():
        assert path.read_bytes() == before, path
    assert not (home / f".pi/agent/extensions/{BIN}-status.ts").exists()
    assert not (home / f".copilot/hooks/{BIN}-status.json").exists()
    assert (state / "agents/upstream.json").read_text() == "{}"
