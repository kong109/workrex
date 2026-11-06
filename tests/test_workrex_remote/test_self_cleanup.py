"""Remote cleanup preserves the local Rex caller and unrelated sessions."""

import shlex
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

import pytest
import yaml

from ..rex_env import RexEnvironment
from ..conftest import poll_until
from .conftest import (
    LOOPBACK,
    Node,
    agent_dirs_env,
    git,
    local_records,
    observer_owner_running,
    wm,
    write_script,
)
from .test_homecoming import commit_on_node, configure, node_worktree


@pytest.mark.parametrize("pending_count", [3, 4])
def test_batch_pending_cleanup_attempts_each_hook_once(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    pending_count: int,
):
    repo = mux_repo_path
    attempts = node.root / "attempts"
    block = node.root / "block"
    configure(
        repo,
        pre_remove=[
            f'case "$(pwd -P)" in {node.root}/*) '
            f"echo attempt >> {attempts}; sleep 0.1; test ! -e {block} ;; esac"
        ],
    )
    block.touch()
    handles = ("one", "two", "three", "unrelated")[:pending_count]
    for handle in handles:
        wm(mux_server, workrex_exe_path, repo, f"add {handle} --remote lo")
        commit_on_node(node_worktree(node, handle), f"{handle}.txt", "merged\n")
        wm(mux_server, workrex_exe_path, repo, f"merge {handle}", expect_fail=True)
    attempts.write_text("")
    started = time.monotonic()
    result = wm(
        mux_server, workrex_exe_path, repo, "remove one two three", expect_fail=True
    )
    count = len(attempts.read_text().splitlines())
    print(f"batch cleanup: {count} hooks, {time.monotonic() - started:.3f}s")
    assert "3 remote workspace(s) could not be removed" in result.stderr
    assert len(local_records(mux_server)) == pending_count
    assert count == pending_count
    block.unlink()
    wm(mux_server, workrex_exe_path, repo, "remove " + " ".join(handles))
    assert local_records(mux_server) == []
    assert node.workspaces() == []


@pytest.mark.parametrize("operation", ["remove", "merge"])
def test_native_cleanup_preserves_local_session(
    mux_server, workrex_exe_path, mux_repo_path, node, operation
):
    configure(mux_repo_path)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    workspace = node.workspace("feat")
    remote_session = workspace["execution"]["rex"]["sessions"]["0"]
    if operation == "merge":
        commit_on_node(Path(workspace["worktree_path"]), "work.txt", "merged\n")
    result = wm(mux_server, workrex_exe_path, mux_repo_path, f"{operation} feat")
    assert "remote workspace" in result.stdout
    sessions = {
        session["session_id"] for session in node.rex.api("session.list")["sessions"]
    }
    assert mux_server.session_id in sessions and remote_session not in sessions
    assert node.workspaces() == [] and local_records(mux_server) == []
    if operation == "merge":
        assert git(mux_repo_path, "show", "main:work.txt") == "merged"


def test_failed_native_cleanup_is_retryable(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    block = node.root / "block-remove"
    configure(
        mux_repo_path,
        pre_remove=[f'case "$(pwd -P)" in {node.root}/*) test ! -e {block} ;; esac'],
    )
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    before = node.workspace("feat")
    block.touch()
    wm(mux_server, workrex_exe_path, mux_repo_path, "remove feat", expect_fail=True)
    pending = node.workspace("feat")
    assert pending["ws_id"] == before["ws_id"]
    assert Path(pending["worktree_path"]).exists()
    assert local_records(mux_server)
    block.unlink()
    wm(mux_server, workrex_exe_path, mux_repo_path, "remove feat")
    assert not node.workspaces() and not local_records(mux_server)


def test_multi_remove_preserves_failed_workspace(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    configure(mux_repo_path)
    for handle in ["first", "dirty", "last"]:
        wm(mux_server, workrex_exe_path, mux_repo_path, f"add {handle} --remote lo")
    (node_worktree(node, "dirty") / "untracked.txt").write_text("keep me\n")
    result = wm(
        mux_server,
        workrex_exe_path,
        mux_repo_path,
        "remove first dirty last",
        expect_fail=True,
    )
    assert "1 remote workspace(s) could not be removed" in result.stderr
    assert [workspace["handle"] for workspace in node.workspaces()] == ["dirty"]
    assert [record["handle"] for record in local_records(mux_server)] == ["dirty"]
    assert len(node.sessions()) == 1


def test_remote_remove_does_not_require_the_callers_rex_server(
    mux_server: RexEnvironment, workrex_exe_path: Path, mux_repo_path: Path
):
    configure(mux_repo_path)
    with tempfile.TemporaryDirectory(prefix="wr-separated-", dir="/tmp") as directory:
        base = Path(directory).resolve()
        remote_path = base / "remote-rex"
        remote_path.mkdir()
        remote = RexEnvironment(remote_path)
        remote.start_server()
        hosted = Node(base / "node", remote)
        wrapper = write_script(
            base / "node-binary",
            f"export REX_SERVER={shlex.quote(remote.server_url)}\n"
            "unset REX_SESSION REX_BLOCK\n"
            f'exec {shlex.quote(str(workrex_exe_path))} "$@"\n',
        )
        config_path = mux_server.home_path / ".config/workrex/config.yaml"
        config_path.write_text(
            yaml.safe_dump(
                {
                    "nerdfont": False,
                    "remotes": {
                        "lo": {
                            "host": "localhost",
                            "path": str(hosted.root),
                            "binary": str(wrapper),
                            "isolation": "none",
                            "terminal": "rex",
                        }
                    },
                }
            )
        )
        try:
            wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
            hosted.wait_for_agent(mux_server, "feat", "AGENT-READY")
            placement = hosted.workspace("feat")["execution"]["rex"]
            assert placement["host_label"] == remote.server_url
            local_session = mux_server.session_id
            local_block = mux_server.runner_block_id
            assert local_session is not None and local_block is not None
            mux_server.stop_server()
            removed = subprocess.run(
                [str(workrex_exe_path), "remove", "--force", "feat"],
                cwd=mux_repo_path,
                env={
                    **mux_server.env,
                    **LOOPBACK,
                    **agent_dirs_env(mux_server.home_path),
                    "REX_SERVER": mux_server.server_url,
                    "REX_SESSION": local_session,
                    "REX_BLOCK": local_block,
                },
                capture_output=True,
                text=True,
                timeout=30,
            )
            assert removed.returncode == 0, removed.stdout + removed.stderr
            assert "Removed remote workspace 'feat'" in removed.stdout
            assert hosted.workspaces() == [] and local_records(mux_server) == []
            assert remote.server is not None and remote.server.poll() is None
        finally:
            try:
                records = (
                    mux_server.home_path / ".local/state/workrex/remote/lo/workspaces"
                )
                shutil.rmtree(records, ignore_errors=True)
                assert poll_until(
                    lambda: not observer_owner_running(mux_server), timeout=10
                )
                hosted.stop_sessions()
            finally:
                remote.stop_server()
