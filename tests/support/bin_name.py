"""The binary name the fork installs, and the `rebrand!` rename of embedded text."""

import json
import re
from pathlib import Path

BIN = "workrex"


def rebrand(text: str) -> str:
    """Apply the binary rename that `rebrand!` applies to embedded hook and skill text."""
    return re.sub(
        r"(?:(?<![\w./@:-])|(?<=[\s\"'`(]/))workmux(?![\w/@:-]|\.[^\W_])", BIN, text
    )


def bundled(path: Path) -> str:
    """A bundled source as `setup` installs it."""
    return rebrand(path.read_text())


def hook_bin() -> str:
    """The command the bundled Claude Code status hooks run, as `setup` installs them."""
    plugin = Path(__file__).parents[2] / ".claude-plugin" / "plugin.json"
    hooks = json.loads(rebrand(plugin.read_text()))["hooks"]
    return hooks["Stop"][0]["hooks"][0]["command"].split()[0]
