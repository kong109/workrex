"""Native remote shell identity and isolation over opt-in real SSH."""

import pytest
import yaml

from .conftest import commit_project
from . import test_ssh_workflows as workflows

pytestmark = workflows.pytestmark
ssh_workspace = workflows.ssh_workspace


@pytest.mark.parametrize("container", [False, True])
def test_real_ssh_shell(ssh_workspace, container):
    ws = ssh_workspace
    if container:
        if ws.shell("docker info >/dev/null 2>&1; printf %s $?") != "0":
            pytest.skip("configured SSH host has no Docker daemon")
        path = ws.env.home_path / ".config/workrex/config.yaml"
        config = yaml.safe_load(path.read_text())
        config["remotes"]["lo"]["isolation"] = "container"
        path.write_text(yaml.safe_dump(config))
    commit_project(ws.repo, {"panes": [{"command": "echo AGENT-READY"}]})
    ws.run("add feat --remote lo")
    ws.wait_ready("feat", "AGENT-READY")
    before = ws.record("feat")["execution"]["rex"]
    opened = ws.node_shell("feat", "open", "shell1")
    assert opened["container"] is container
    assert opened["session_id"].startswith("session:")
    again = ws.node_shell("feat", "open", "shell1")
    assert again["session_id"] == opened["session_id"]
    host = ws.node_shell("feat", "open", "shell2", host=True)
    assert host["container"] is False
    view = ws.rex_api("session.view", {"session_id": host["session_id"]})
    block = view["windows"][0]["focused_block_id"]
    ws.block_call(
        block, "write", {"data": "printf 'host shell\\n' > host-shell-marker\r"}
    )
    ws.wait_ready("feat", "AGENT-READY")
    assert ws.node_shell("feat", "close", "shell1")["closed"] is True
    assert ws.node_shell("feat", "close", "shell2")["closed"] is True
    assert ws.record("feat")["execution"]["rex"]["blocks"] == before["blocks"]


def test_gc_preserves_interactive_shell_after_configured_blocks_close(ssh_workspace):
    ws = ssh_workspace
    commit_project(ws.repo, {"panes": [{"command": "echo AGENT-READY"}]})
    ws.run("add feat --remote lo")
    ws.wait_ready("feat", "AGENT-READY")
    opened = ws.node_shell("feat", "open", "shell1")
    session = ws.record("feat")["execution"]["rex"]["sessions"]["0"]
    ws.rex_api("session.destroy", {"session_id": session})
    report = ws.run("remote gc lo --dry-run")
    assert "workspace 'feat'" not in report.stdout
    assert (
        ws.rex_api("session.view", {"session_id": opened["session_id"]})["session_id"]
        == opened["session_id"]
    )
    ws.node_shell("feat", "close", "shell1")
