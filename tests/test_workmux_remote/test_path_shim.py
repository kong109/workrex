"""`remote setup` puts the node's pinned binary on the login PATH through
`~/.local/bin/workrex`, run here by the loopback transport's local `sh`."""

import json
import os
import pty
import select
import subprocess
import time
from pathlib import Path
from typing import List

from ..support.bin_name import BIN
from .conftest import RemoteCli, dev_pin, write_script

MARKER = f".{BIN}-node"
PROFILE = 'PATH="$HOME/.local/bin:$PATH"\nexport PATH\n'


def configure(
    cli: RemoteCli, binary: Path, profile: bool = True, isolation: str = "none"
) -> Path:
    """One loopback remote on the host; returns the shim path."""
    cli.write_config(
        {
            "lo": {
                "host": "localhost",
                "path": "~/node",
                "binary": str(binary),
                "isolation": isolation,
            }
        }
    )
    cli.env["SHELL"] = "/bin/sh"
    if profile:
        (cli.home / ".profile").write_text(PROFILE)
    return cli.home / ".local" / "bin" / BIN


def login_shell(cli: RemoteCli, command: str) -> List[str]:
    """Run `command` in the node user's login shell, outside any execution."""
    env = {
        k: v for k, v in cli.env.items() if k not in ("WM_NODE_ROOT", "XDG_STATE_HOME")
    }
    result = subprocess.run(
        ["/bin/sh", "-l", "-c", command],
        cwd=cli.home,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    return result.stdout.splitlines()


def test_setup_puts_the_pin_on_the_login_path_and_repoints_it_after_an_upgrade(
    loopback_cli: RemoteCli, workmux_exe_path: Path, tmp_path: Path
):
    shim = configure(loopback_cli, workmux_exe_path)
    first = dev_pin(workmux_exe_path)

    result = loopback_cli.run("remote", "setup", "lo")

    assert result.returncode == 0, result.stdout + result.stderr
    assert f"✓ lo: {shim} runs {first}" in result.stdout, result.stdout
    assert "! lo" not in result.stdout, result.stdout
    version = loopback_cli.run("--version").stdout.strip()
    found, reported = login_shell(loopback_cli, f"command -v {BIN}; {BIN} --version")
    assert (found, reported) == (str(shim), version)
    assert str(loopback_cli.home / "node" / "bin" / first) in shim.read_text()

    marker = loopback_cli.home / "node" / "bin" / MARKER
    assert marker.is_file()
    marker.unlink()  # as in a root set up before the marker existed

    rerun = loopback_cli.run("remote", "setup", "lo")
    assert f"✓ lo: {shim} already runs {first}" in rerun.stdout, rerun.stdout
    assert "is already installed" in rerun.stdout, rerun.stdout
    assert marker.is_file()
    doctor = loopback_cli.run("remote", "doctor", "lo")
    assert f"✓ path       {shim} runs {first} in login shells" in doctor.stdout, (
        doctor.stdout
    )

    # A new build has new bytes, so a new pin.
    upgraded = tmp_path / BIN
    upgraded.write_bytes(workmux_exe_path.read_bytes() + b"upgrade")
    upgraded.chmod(0o755)
    configure(loopback_cli, upgraded)
    second = dev_pin(upgraded)

    result = loopback_cli.run("remote", "setup", "lo")

    assert result.returncode == 0, result.stdout + result.stderr
    assert f"✓ lo: {shim} runs {second}" in result.stdout, result.stdout
    text = shim.read_text()
    assert str(loopback_cli.home / "node" / "bin" / second) in text
    assert first not in text
    assert login_shell(loopback_cli, f"command -v {BIN}") == [str(shim)]

    # Another machine's setup pointed the shim at its own pin.
    shim.write_text(text.replace(second, first))
    stale = loopback_cli.run("remote", "doctor", "lo")
    old_pin = loopback_cli.home / "node" / "bin" / first
    assert f"! path       {shim} runs {old_pin}, not {second}" in stale.stdout, (
        stale.stdout
    )

    # Its root, or that pin, is gone: every login-shell `workrex` fails.
    old_pin.unlink()
    broken = loopback_cli.run("remote", "doctor", "lo")
    assert (
        f"! path       {shim} runs {old_pin}, which is missing or not executable"
        in broken.stdout
    ), broken.stdout


def test_a_marker_that_cannot_be_written_warns_and_setup_goes_on(
    loopback_cli: RemoteCli, workmux_exe_path: Path
):
    shim = configure(loopback_cli, workmux_exe_path)
    marker = loopback_cli.home / "node" / "bin" / MARKER
    marker.mkdir(parents=True)  # not a file, so it can't be written
    warning = (
        f"! lo: could not write ~/node/bin/{MARKER}, so `{BIN} update` on the server "
        "may replace the pinned binaries"
    )

    fresh = loopback_cli.run("remote", "setup", "lo")
    rerun = loopback_cli.run("remote", "setup", "lo")

    for result in (fresh, rerun):
        assert result.returncode == 0, result.stdout + result.stderr
        assert warning in result.stdout, result.stdout
        assert f"✓ lo: {shim} " in result.stdout, result.stdout
    assert "installed" in fresh.stdout and "already installed" in rerun.stdout
    assert (loopback_cli.home / "node" / "bin" / dev_pin(workmux_exe_path)).is_file()


def test_setup_takes_over_a_marked_shim_and_leaves_any_other_alone(
    loopback_cli: RemoteCli, workmux_exe_path: Path
):
    shim = configure(loopback_cli, workmux_exe_path)
    shim.parent.mkdir(parents=True)
    shim.write_text(
        f"#!/bin/sh\n# Written by {BIN} remote setup.\n"
        f'exec /home/u/.{BIN}-node/bin/{BIN}-0.1.0 "$@"\n'
    )

    taken = loopback_cli.run("remote", "setup", "lo")

    assert taken.returncode == 0, taken.stdout + taken.stderr
    assert f"✓ lo: {shim} runs {dev_pin(workmux_exe_path)}" in taken.stdout
    assert f"{BIN}-0.1.0" not in shim.read_text()

    own = f'#!/bin/sh\nexec /opt/{BIN} "$@"\n'
    shim.write_text(own)

    left = loopback_cli.run("remote", "setup", "lo")

    assert left.returncode == 0, left.stdout + left.stderr
    assert (
        f"- lo: left {shim} alone; it was not written by `{BIN} remote setup`"
        in left.stdout
    ), left.stdout
    assert shim.read_text() == own


def test_setup_hints_when_login_shells_do_not_reach_the_shim(
    loopback_cli: RemoteCli, workmux_exe_path: Path
):
    shim = configure(loopback_cli, workmux_exe_path, profile=False)
    loopback_cli.env["PATH"] = "/usr/bin:/bin"

    result = loopback_cli.run("remote", "setup", "lo")

    assert result.returncode == 0, result.stdout + result.stderr
    assert (
        f"! lo: login shells do not find {shim}; add {shim.parent} to PATH in your "
        "shell profile"
    ) in result.stdout, result.stdout
    assert shim.is_file()

    (loopback_cli.home / "opt").mkdir()
    other = write_script(loopback_cli.home / "opt" / BIN, "echo other\n")
    (loopback_cli.home / ".profile").write_text(
        'PATH="$HOME/opt:$HOME/.local/bin:$PATH"\nexport PATH\n'
    )

    shadowed = loopback_cli.run("remote", "setup", "lo")

    assert (
        f"! lo: login shells run {other}, not {shim}; put {shim.parent} earlier on "
        "PATH in your shell profile"
    ) in shadowed.stdout, shadowed.stdout


def test_a_path_entry_spelled_differently_still_reaches_the_shim(
    loopback_cli: RemoteCli, workmux_exe_path: Path
):
    """A symlinked or trailing-slash PATH entry names the shim by another path."""
    shim = configure(loopback_cli, workmux_exe_path, profile=False)
    shim.parent.mkdir(parents=True)
    (loopback_cli.home / "alias").symlink_to(shim.parent)
    (loopback_cli.home / ".profile").write_text(
        'PATH="$HOME/alias/:$PATH"\nexport PATH\n'
    )

    result = loopback_cli.run("remote", "setup", "lo")
    doctor = loopback_cli.run("remote", "doctor", "lo")

    assert result.returncode == 0, result.stdout + result.stderr
    assert "! lo" not in result.stdout, result.stdout
    assert f"✓ path       {shim} runs " in doctor.stdout, doctor.stdout


def test_docker_mode_writes_no_shim_and_notes_one_left_by_server_mode(
    loopback_cli: RemoteCli, workmux_exe_path: Path
):
    shim = configure(loopback_cli, workmux_exe_path, isolation="container")

    setup = loopback_cli.run("remote", "setup", "lo")
    doctor = loopback_cli.run("remote", "doctor", "lo")

    assert setup.returncode == 0, setup.stdout + setup.stderr
    assert not shim.exists() and not shim.is_symlink()
    assert (
        f"- path       Docker mode puts no {BIN} on the login PATH" in doctor.stdout
    ), doctor.stdout

    configure(loopback_cli, workmux_exe_path)
    assert loopback_cli.run("remote", "setup", "lo").returncode == 0
    written = shim.read_text()
    configure(loopback_cli, workmux_exe_path, isolation="container")

    setup = loopback_cli.run("remote", "setup", "lo")
    doctor = loopback_cli.run("remote", "doctor", "lo")

    note = (
        f"{shim} was written by an earlier server-mode setup; remove it if no "
        "server-mode remote on this host uses it"
    )
    assert f"- lo: {note}" in setup.stdout, setup.stdout
    assert f"- path       {note}" in doctor.stdout, doctor.stdout
    assert shim.read_text() == written


def test_update_refuses_to_replace_a_node_pin(
    loopback_cli: RemoteCli, workmux_exe_path: Path
):
    """`update` run through the shim sees the pin as its own executable."""
    pin = loopback_cli.home / "node" / "bin" / f"{BIN}-0.1.0"
    pin.parent.mkdir(parents=True)
    (pin.parent / MARKER).write_text("")
    pin.write_bytes(workmux_exe_path.read_bytes())
    pin.chmod(0o755)
    before = pin.read_bytes()

    result = subprocess.run(
        [str(pin), "update"],
        env=loopback_cli.env,
        capture_output=True,
        text=True,
        timeout=30,
    )

    assert result.returncode != 0
    assert f"{pin.resolve()} is a remote node's pinned binary" in result.stderr, (
        result.stderr
    )
    assert f"run `{BIN} remote setup <remote>` there" in result.stderr
    assert pin.read_bytes() == before


def notice(cli: RemoteCli, exe: Path) -> str:
    """What `exe add` prints on a terminal while a newer release is cached."""
    cache = cli.home / ".cache" / "workmux" / f"{BIN}-update_check.json"
    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.write_text(
        json.dumps(
            {
                "latest_version": "99.0.0",
                "last_checked": int(time.time()),
                "notify_count": 0,
            }
        )
    )
    env = {k: v for k, v in cli.env.items() if k != "WORKMUX_NO_UPDATE_CHECK"}
    leader, follower = pty.openpty()
    proc = subprocess.Popen(
        [str(exe), "add", "x"],
        cwd=cli.cwd,
        env=env,
        stdin=follower,
        stdout=follower,
        stderr=follower,
    )
    os.close(follower)
    output = b""
    deadline = time.time() + 30
    while time.time() < deadline:
        ready, _, _ = select.select([leader], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(leader, 4096)
            except OSError:
                break
            if not chunk:
                break
            output += chunk
        elif proc.poll() is not None:
            break
    proc.wait(timeout=10)
    os.close(leader)
    return output.decode(errors="replace")


def test_node_pins_show_no_update_notice_and_versioned_installs_do(
    loopback_cli: RemoteCli, workmux_exe_path: Path
):
    # workrex has run here before, as on any machine with a versioned install.
    (loopback_cli.home / ".local" / "state" / "workmux").mkdir(parents=True)
    installs = loopback_cli.home / ".local" / "bin"
    pins = loopback_cli.home / "node" / "bin"
    for exe in (installs / f"{BIN}-1.2", pins / f"{BIN}-0.1.0"):
        exe.parent.mkdir(parents=True, exist_ok=True)
        exe.write_bytes(workmux_exe_path.read_bytes())
        exe.chmod(0o755)
    (pins / MARKER).write_text("")

    assert "Update available" in notice(loopback_cli, installs / f"{BIN}-1.2")
    assert "Update available" not in notice(loopback_cli, pins / f"{BIN}-0.1.0")
