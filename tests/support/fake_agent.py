FAKE_AGENT = """
import subprocess
import sys
import time
from pathlib import Path

workrex, mode, ready = sys.argv[1:]


def report(status):
    subprocess.run(
        [workrex, "set-window-status", status], check=True, stdin=subprocess.DEVNULL
    )


report("done")
Path(ready).touch()
for line in sys.stdin:
    if mode == "exit":
        sys.exit(0)
    if mode == "turns":
        time.sleep(1.5)
        report("working")
        time.sleep(1.5)
        report("done")
"""
