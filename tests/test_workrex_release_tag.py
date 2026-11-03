"""scripts/check-release-tag.sh accepts only fork tags that match Cargo.toml."""

import subprocess
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parent.parent / "scripts/check-release-tag.sh"


def check(tag: str, version: str, tmp_path: Path) -> subprocess.CompletedProcess:
    cargo = tmp_path / "Cargo.toml"
    cargo.write_text(f'[package]\nname = "workmux"\nversion = "{version}"\n')
    return subprocess.run(
        [SCRIPT, tag, cargo], capture_output=True, text=True, env={"PATH": "/usr/bin:/bin"}
    )


@pytest.mark.parametrize("tag", ["v0.1.278-rex.1", "v0.1.278-rex.10"])
def test_fork_tag_matching_cargo_passes(tag: str, tmp_path: Path):
    result = check(tag, tag[1:], tmp_path)
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize(
    ("tag", "version"),
    [
        ("v0.1.278", "0.1.278"),
        ("v0.1.278-rex.2", "0.1.278-rex.1"),
        ("v0.1.278-rex", "0.1.278-rex"),
        ("v0.1.278-rex.0", "0.1.278-rex.0"),
        ("v0.1.278-rex.01", "0.1.278-rex.01"),
        ("0.1.278-rex.1", "0.1.278-rex.1"),
        ("", "0.1.278-rex.1"),
    ],
)
def test_upstream_or_mismatched_tags_fail(tag: str, version: str, tmp_path: Path):
    result = check(tag, version, tmp_path)
    assert result.returncode == 1, result.stdout
