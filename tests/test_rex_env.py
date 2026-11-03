"""The Rex test environment cleans up the server it starts."""

from pathlib import Path

import pytest

from .rex_env import RexEnvironment, rex_binary


@pytest.mark.skipif(rex_binary() is None, reason="rex CLI not found")
def test_a_failed_start_stops_the_server(tmp_path: Path, monkeypatch):
    env = RexEnvironment(tmp_path)
    launched = []

    def failing_api(self, method, args):
        launched.append(self.server)
        raise RuntimeError(f"{method} failed")

    monkeypatch.setattr(RexEnvironment, "api", failing_api)
    try:
        with pytest.raises(RuntimeError):
            env.start_server()
        assert launched[0].poll() is not None
    finally:
        if launched and launched[0].poll() is None:
            launched[0].kill()
            launched[0].wait(timeout=5)
