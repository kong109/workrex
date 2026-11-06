#!/usr/bin/env python3
"""Reject stale operational branding and unsupported terminal instructions.

The exact original copyright attribution is preserved in LICENSE.
"""

import argparse
import json
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parent.parent
LEGACY_BRAND = re.compile(r"work[m]ux", re.IGNORECASE)
FORBIDDEN = re.compile(
    r"work[m]ux|t[m]ux|wez[t]erm|zel[l]ij|\balias +w[m] *=|\bW[M]_", re.IGNORECASE
)


def owned_files():
    names = (
        subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            cwd=ROOT,
        )
        .decode()
        .split("\0")
    )
    for name in sorted(set(filter(None, names))):
        if name.startswith(".audit/"):
            continue
        path = ROOT / name
        if path.is_file():
            yield name, path


def audit():
    findings = []
    for name, path in owned_files():
        if FORBIDDEN.search(name):
            findings.append(
                {"path": name, "line": 0, "text": "unsupported name in path"}
            )
        try:
            content = path.read_text()
        except UnicodeDecodeError:
            continue
        # Lockfile package names belong to third-party optional UI libraries.
        pattern = LEGACY_BRAND if name == "Cargo.lock" else FORBIDDEN
        for number, line in enumerate(content.splitlines(), 1):
            if name == "LICENSE" and re.fullmatch(
                r"Copyright \(c\) 2025 work[m]ux contributors", line
            ):
                continue
            if pattern.search(line):
                findings.append({"path": name, "line": number, "text": line.strip()})
    return findings


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--json", action="store_true", help="Print every finding as JSON"
    )
    args = parser.parse_args()
    findings = audit()
    if args.json:
        print(json.dumps(findings, indent=2))
    else:
        for finding in findings[:25]:
            print(f"{finding['path']}:{finding['line']}: {finding['text']}")
        print(
            f"{len(findings)} stale references across {len({f['path'] for f in findings})} files"
        )
    raise SystemExit(bool(findings))
