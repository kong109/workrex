"""`remote setup` puts the node's pinned binary on the login PATH through
`~/.local/bin/workrex`, run here by the loopback transport's local `sh`."""

import subprocess
from pathlib import Path
from typing import List

from ..support.bin_name import BIN
from .conftest import RemoteCli, dev_pin

PROFILE = 'PATH="$HOME/.local/bin:$PATH"\nexport PATH\n'


def configure(cli: RemoteCli, binary: Path, profile: bool = True) -> Path:
    """One loopback remote on the host; returns the shim path."""
    cli.write_config(
        {
            "lo": {
                "host": "localhost",
                "path": "~/node",
                "binary": str(binary),
                "isolation": "none",
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

    rerun = loopback_cli.run("remote", "setup", "lo")
    assert f"✓ lo: {shim} already runs {first}" in rerun.stdout, rerun.stdout
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


def test_update_refuses_to_replace_a_node_pin(
    loopback_cli: RemoteCli, workmux_exe_path: Path
):
    """`update` run through the shim sees the pin as its own executable."""
    pin = loopback_cli.home / "node" / "bin" / f"{BIN}-0.1.0"
    pin.parent.mkdir(parents=True)
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
