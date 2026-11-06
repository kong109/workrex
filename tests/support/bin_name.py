"""The installed binary name and the actual bundled skill and hook text."""

import json
from pathlib import Path

BIN = "workrex"


def bundled(path: Path) -> str:
    return path.read_text()


def hook_bin() -> str:
    plugin = Path(__file__).parents[2] / ".claude-plugin" / "plugin.json"
    hooks = json.loads(plugin.read_text())["hooks"]
    return hooks["Stop"][0]["hooks"][0]["command"].split()[0]
