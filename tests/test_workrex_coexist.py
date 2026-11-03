"""workrex coexists with upstream workmux: setup and uninstall leave upstream's hooks alone."""

import json
import os
import re
import shlex
import shutil
import subprocess
import time
from pathlib import Path

import pytest

from .conftest import (
    MuxEnvironment,
    get_scripts_dir,
    make_env_script,
    poll_until,
    poll_until_file_has_content,
)
from .support.bin_name import BIN, bundled, hook_bin, rebrand
from .support.setup import (
    install_opencode_version,
    run_setup_interactive,
    run_setup_with_answers,
)

REPO = Path(__file__).parent.parent

UPSTREAM_STOP = "workmux set-window-status done"
UPSTREAM_WORKING = "workmux set-window-status working"
AGY_SUFFIX = " >/dev/null 2>&1 || true; printf '{}\\n'"
SKILL_DIRS = (
    ".claude/skills",
    ".config/opencode/skills",
    ".grok/skills",
    ".pi/agent/skills",
    ".omp/agent/skills",
)


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
        home / ".grok/hooks/workmux-status.json": pretty(
            json.loads((REPO / ".grok/hooks/workmux-status.json").read_text())
        ),
        home / ".omp/agent/extensions/workmux-status.ts": (
            REPO / "resources/omp/extensions/workmux-status.ts"
        ).read_text(),
        home / ".gemini/config/hooks.json": pretty(
            {
                "workmux-status": {
                    "PreInvocation": [
                        {"type": "command", "command": f"{call}{AGY_SUFFIX}"}
                        for call in ("workmux register-agent", UPSTREAM_WORKING)
                    ],
                    "Stop": [
                        {"type": "command", "command": f"{UPSTREAM_STOP}{AGY_SUFFIX}"}
                    ],
                }
            }
        ),
    }
    (home / ".gemini/antigravity-cli").mkdir(parents=True, exist_ok=True)
    for path, text in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return {path: path.read_bytes() for path in files}


def run_uninstall(env: MuxEnvironment, exe: Path, exit_code: int = 0) -> str:
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
    assert (home / f".grok/hooks/{BIN}-status.json").is_file()
    assert (home / f".omp/agent/extensions/{BIN}-status.ts").is_file()
    opencode = home / f".config/opencode/plugins/{BIN}-status.ts"
    assert f"{BIN} set-window-status" in opencode.read_text()
    agy = home / ".gemini/config/hooks.json"
    assert set(json.loads(agy.read_text())) == {"workmux-status", f"{BIN}-status"}
    shared.append(agy)
    for path, before in upstream.items():
        if path not in shared:
            assert path.read_bytes() == before, path

    run_uninstall(mux_server, workmux_exe_path)

    for path, before in upstream.items():
        assert path.read_bytes() == before, path
    assert not (home / f".pi/agent/extensions/{BIN}-status.ts").exists()
    assert not (home / f".copilot/hooks/{BIN}-status.json").exists()
    assert not (home / f".grok/hooks/{BIN}-status.json").exists()
    assert not (home / f".omp/agent/extensions/{BIN}-status.ts").exists()
    assert not opencode.exists()
    assert (state / "agents/upstream.json").read_text() == "{}"


# Upstream workmux to test against, for example a build of this repo's base commit.
# Without one, the tests stand in for it: upstream's skills are this repo's skills/ texts.
UPSTREAM_EXE = os.environ.get("WORKREX_TEST_UPSTREAM_EXE")


def upstream_skills(home: Path) -> list[Path]:
    return [
        home / skills_dir / skill.name / "SKILL.md"
        for skills_dir in SKILL_DIRS
        for skill in sorted((REPO / "skills").iterdir())
    ]


def setup_upstream(env: MuxEnvironment) -> dict[Path, bytes]:
    """Run upstream `workmux setup`, answering yes; return its skill files' bytes."""
    home = env.home_path
    if UPSTREAM_EXE:
        for skills_dir in SKILL_DIRS:
            (home / skills_dir).parent.mkdir(parents=True, exist_ok=True)
        run_setup_answering(env, Path(UPSTREAM_EXE))
    else:
        for path in upstream_skills(home):
            path.parent.mkdir(parents=True, exist_ok=True)
            text = (REPO / "skills" / path.parent.name / "SKILL.md").read_text()
            path.write_text(text)
    return {path: path.read_bytes() for path in upstream_skills(home)}


def uninstall_upstream(env: MuxEnvironment) -> None:
    """Run upstream `workmux uninstall`.

    It deletes each skill directory whose SKILL.md still holds upstream's text,
    then its whole cache and state directories.
    """
    home = env.home_path
    if UPSTREAM_EXE:
        run_uninstall(env, Path(UPSTREAM_EXE))
        return
    for skills_dir in SKILL_DIRS:
        base = home / skills_dir
        for skill in (REPO / "skills").iterdir():
            path = base / skill.name / "SKILL.md"
            if path.is_file() and path.read_text() == (skill / "SKILL.md").read_text():
                shutil.rmtree(path.parent)
        if base.is_dir() and not any(base.iterdir()):
            base.rmdir()
    for xdg_dir in (".cache", ".local/state"):
        shutil.rmtree(home / xdg_dir / "workmux", ignore_errors=True)


def run_setup_answering(env: MuxEnvironment, exe: Path, overwrite: str = "y") -> int:
    """Run setup answering yes to hooks and skills and `overwrite` to each skill-overwrite prompt."""
    done = run_setup_interactive(env, exe)
    overwrites = 0
    deadline = time.monotonic() + 30
    while not (done.exists() and done.read_text().strip()):
        assert time.monotonic() < deadline, env.capture_pane("test")
        screen = env.capture_pane("test") or ""
        lines = [line for line in screen.splitlines() if line.strip()]
        if lines and "?" in lines[-1] and lines[-1].endswith("]"):
            prompt = "with bundled version?" in lines[-1]
            env.send_keys("test:", overwrite if prompt else "y")
            overwrites += prompt
            poll_until(lambda: env.capture_pane("test") != screen, 5)
        time.sleep(0.05)
    assert done.read_text().strip() == "0", env.capture_pane("test")
    return overwrites


def skills_summary(output: str) -> str:
    match = re.search(r"Skills for Claude Code \(.*\): (.*)", output)
    assert match, output
    return match.group(1).strip()


def skill_files(home: Path) -> list[Path]:
    return sorted(
        path
        for skills_dir in SKILL_DIRS
        for path in (home / skills_dir).rglob("*")
        if path.is_file()
    )


# Skills whose bundled text differs from upstream's, so setup asks before overwriting them.
DIFFERING = ("merge", "rebase", "worktree", "coordinator")


@pytest.mark.parametrize("answer", ["n", "y"])
def test_uninstall_after_upstream_setup_deletes_only_what_setup_wrote(
    mux_server: MuxEnvironment, workmux_exe_path: Path, answer: str
):
    home = mux_server.home_path
    install_opencode_version(mux_server, "v1.18.29")
    upstream = setup_upstream(mux_server)

    overwrites = run_setup_answering(mux_server, workmux_exe_path, answer)

    assert overwrites == len(DIFFERING) * len(SKILL_DIRS)
    for skills_dir in SKILL_DIRS:
        ours = home / skills_dir / BIN / "SKILL.md"
        assert ours.read_text() == bundled(REPO / "skills/workmux/SKILL.md"), ours

    output = run_uninstall(mux_server, workmux_exe_path)

    # A "y" overwrite is deleted, not restored; open-pr's text was already there.
    assert skills_summary(output) == (
        "1 removed, 5 kept" if answer == "n" else "5 removed, 1 kept"
    )
    for path, before in upstream.items():
        if answer == "y" and path.parent.name in DIFFERING:
            assert not path.parent.exists(), path
        else:
            assert path.read_bytes() == before, path
    for skills_dir in SKILL_DIRS:
        assert not (home / skills_dir / BIN).exists(), skills_dir


@pytest.mark.parametrize("first", [BIN, "workmux"])
def test_both_setups_leave_nothing_once_both_uninstall(
    mux_server: MuxEnvironment, workmux_exe_path: Path, first: str
):
    home = mux_server.home_path
    install_opencode_version(mux_server, "v1.18.29")
    for skills_dir in SKILL_DIRS:
        (home / skills_dir).parent.mkdir(parents=True, exist_ok=True)
    run_setup_answering(mux_server, workmux_exe_path)
    setup_upstream(mux_server)
    assert run_setup_answering(mux_server, workmux_exe_path) > 0
    own_state = home / ".local/state" / BIN
    assert (own_state / "skills.json").is_file()

    if first == BIN:
        output = run_uninstall(mux_server, workmux_exe_path)
        # open-pr holds the text workrex wrote, so it goes too: a known gap.
        assert skills_summary(output) == "6 removed"
        uninstall_upstream(mux_server)
    else:
        uninstall_upstream(mux_server)
        output = run_uninstall(mux_server, workmux_exe_path)
        assert skills_summary(output) == "5 removed"

    assert skill_files(home) == []
    assert not own_state.exists()


@pytest.mark.parametrize("first", [BIN, "workmux"])
@pytest.mark.parametrize("edited", ["after-setup", "before-setup-answered-no"])
def test_a_merge_skill_the_user_edited_survives_both_uninstalls(
    mux_server: MuxEnvironment, workmux_exe_path: Path, first: str, edited: str
):
    home = mux_server.home_path
    install_opencode_version(mux_server, "v1.18.29")
    setup_upstream(mux_server)
    merges = [home / skills_dir / "merge/SKILL.md" for skills_dir in SKILL_DIRS]
    edit = "the user's merge skill\n"
    if edited == "after-setup":
        run_setup_answering(mux_server, workmux_exe_path, "y")
        for merge in merges:
            merge.write_text(edit)
    else:
        for merge in merges:
            merge.write_text(edit)
        run_setup_answering(mux_server, workmux_exe_path, "n")

    if first == BIN:
        run_uninstall(mux_server, workmux_exe_path)
        uninstall_upstream(mux_server)
    else:
        uninstall_upstream(mux_server)
        run_uninstall(mux_server, workmux_exe_path)

    for merge in merges:
        assert merge.read_text() == edit, merge
    assert skill_files(home) == sorted(merges)


def test_uninstall_errors_name_the_state_directory_and_the_update_cache(
    mux_server: MuxEnvironment, workmux_exe_path: Path
):
    if os.geteuid() == 0:
        pytest.skip("root ignores directory permissions")
    home = mux_server.home_path
    # A directory without write permission keeps what is inside it.
    locked_state = home / ".local/state" / BIN / "locked"
    (locked_state / "inner").mkdir(parents=True)
    cache = home / ".cache/workmux"
    cache.mkdir(parents=True)
    (cache / f"{BIN}-update_check.json").write_text("{}")
    for locked in (locked_state, cache):
        locked.chmod(0o555)
    try:
        output = run_uninstall(mux_server, workmux_exe_path, exit_code=1)
    finally:
        for locked in (locked_state, cache):
            locked.chmod(0o755)

    denied = os.strerror(13)
    assert (
        f"uninstall completed with 2 error(s): State directory: {denied} (os error 13); "
        f"Update cache: {denied} (os error 13)"
    ) in output


def run_update_check(env: MuxEnvironment, exe: Path) -> None:
    """Run the background update check against a fake GitHub that reports v0.1.300."""
    fake_bin = env.home_path / "fake-bin"
    fake_bin.mkdir(exist_ok=True)
    curl = fake_bin / "curl"
    curl.write_text('#!/bin/sh\necho \'{"tag_name": "v0.1.300"}\'\n')
    curl.chmod(0o755)
    subprocess.run(
        [str(exe), "_check-update"],
        env=env.env
        | {
            "PATH": f"{fake_bin}:{env.env['PATH']}",
            "XDG_CACHE_HOME": str(env.home_path / ".cache"),
        },
        check=True,
        timeout=30,
    )


def test_update_check_keeps_its_own_cache_beside_upstreams_until_uninstall(
    mux_server: MuxEnvironment, workmux_exe_path: Path
):
    cache = mux_server.home_path / ".cache/workmux"
    cache.mkdir(parents=True)
    upstream = cache / "update_check.json"
    upstream.write_text('{"latest_version": "9.9.9", "last_checked": 1}')
    before = upstream.read_bytes()

    run_update_check(mux_server, workmux_exe_path)

    ours = json.loads((cache / f"{BIN}-update_check.json").read_text())
    assert ours["latest_version"] == "0.1.300"
    assert upstream.read_bytes() == before

    run_uninstall(mux_server, workmux_exe_path)

    # Upstream's uninstall removes the cache dir and its own file with it.
    assert not (cache / f"{BIN}-update_check.json").exists()
    assert upstream.read_bytes() == before
