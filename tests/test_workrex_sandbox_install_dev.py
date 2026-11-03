"""`sandbox install-dev` finds, copies and installs the binary cargo builds, named BIN."""

import os
import subprocess
from pathlib import Path

import pytest

from .support.bin_name import BIN

FAKES = {
    # cargo builds target/<triple>/<profile>/BIN, as the [[bin]] target names it.
    "cargo": f"""#!/bin/sh
case "$1" in
  locate-project) echo "$WS/Cargo.toml" ;;
  build) profile=debug; for a in "$@"; do [ "$a" = --release ] && profile=release; done
         mkdir -p "$WS/target/$3/$profile" && echo built > "$WS/target/$3/$profile/{BIN}" ;;
esac
""",
    "rustup": """#!/bin/sh
for a in aarch64 x86_64; do echo $a-unknown-linux-gnu; echo $a-unknown-linux-musl; done
""",
    "docker": """#!/bin/sh
echo "docker $*" >> "$LOG"
if [ "$1" = build ]; then ls "$4" >> "$LOG"; cat "$4/Dockerfile" >> "$LOG"; fi
""",
    "limactl": """#!/bin/sh
echo "limactl $*" >> "$LOG"
[ "$1" = list ] && echo '{"name":"wm-test","status":"Running"}'
exit 0
""",
    "aarch64-unknown-linux-gnu-gcc": "#!/bin/sh\n",
    "x86_64-unknown-linux-gnu-gcc": "#!/bin/sh\n",
}


@pytest.mark.parametrize("skip_build", [False, True])
def test_install_dev_ships_the_built_binary(
    tmp_path: Path, workmux_exe_path: Path, skip_build: bool
):
    fakebin, ws, home = tmp_path / "bin", tmp_path / "ws", tmp_path / "home"
    for d in (fakebin, ws, home):
        d.mkdir()
    for name, script in FAKES.items():
        (fakebin / name).write_text(script)
        (fakebin / name).chmod(0o755)
    log = tmp_path / "log"
    env = {
        "PATH": f"{fakebin}:/usr/bin:/bin",
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_STATE_HOME": str(home / ".state"),
        "WS": str(ws),
        "LOG": str(log),
    }
    if skip_build:
        subprocess.run([fakebin / "cargo", "build", "--target", _guest_target()], env=env)
    args = ["sandbox", "install-dev"] + (["--skip-build"] if skip_build else [])
    result = subprocess.run(
        [workmux_exe_path, *args], env=env, cwd=ws, capture_output=True, text=True
    )

    assert result.returncode == 0, result.stderr + result.stdout
    calls = log.read_text()
    assert f"\nDockerfile\n{BIN}\n" in calls, calls
    assert f"COPY {BIN} /usr/local/bin/{BIN}\n" in calls, calls
    binary = ws / "target" / _guest_target() / "debug" / BIN
    assert f"limactl cp {binary} wm-test:/tmp/workmux.new" in calls, calls
    assert f'/tmp/workmux.new "$HOME/.local/bin/{BIN}"' in calls, calls


def _guest_target() -> str:
    arch = "aarch64" if os.uname().machine in ("arm64", "aarch64") else "x86_64"
    libc = "musl" if os.uname().sysname == "Linux" else "gnu"
    return f"{arch}-unknown-linux-{libc}"
