"""Lifecycle requests typed in the node's own shell, executed by the laptop."""

import shlex
from pathlib import Path

import pytest
import yaml

from ..rex_env import RexEnvironment
from ..conftest import poll_until
from ..support.bin_name import hook_bin
from .conftest import Node, git, local_records, observer_owner_running, wm
from .test_homecoming import commit_on_node, configure, node_worktree
from .test_siblings import node_pane


def type_command(node: Node, ex_id: str, command: str) -> None:
    node.send(f"{ex_id}-0", command)


def request(node: Node, ex_id: str, command: str) -> None:
    type_command(node, ex_id, f"{node.root}/run/bin/{hook_bin()} {command}")


def wait_message(node: Node, ex_id: str, text: str, timeout: int = 30) -> None:
    assert poll_until(lambda: text in node_pane(node, ex_id), timeout=timeout), (
        node_pane(node, ex_id)
    )


def wait_removed(env: RexEnvironment, node: Node) -> None:
    assert poll_until(
        lambda: not local_records(env)
        and not node.workspaces()
        and not node.sessions(),
        timeout=40,
    )


@pytest.mark.parametrize("operation", ["rm", "merge", "merge --rebase"])
def test_node_cleanup(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    operation: str,
):
    configure(mux_repo_path)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    [record] = local_records(mux_server)
    if operation.startswith("merge"):
        commit_on_node(node_worktree(node, "feat"), "work.txt", "merged\n")
    request(node, record["execution"]["ex_id"], operation)
    wait_removed(mux_server, node)
    if operation.startswith("merge"):
        assert git(mux_repo_path, "show", "main:work.txt") == "merged"


def test_unmerged_refuses_then_force(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    configure(mux_repo_path)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    commit_on_node(node_worktree(node, "feat"), "work.txt", "unmerged\n")
    request(node, ex_id, "rm")
    wait_message(node, ex_id, "not merged into")
    assert local_records(mux_server)
    request(node, ex_id, "rm --force")
    wait_removed(mux_server, node)


@pytest.mark.parametrize(
    ("command", "message"),
    [
        ("rm other", "only request an operation on its own handle"),
        ("merge other", "only request an operation on its own handle"),
        ("rm feat feat", "only request an operation on its own handle"),
        ("rm --gone", "does not support"),
        ("rm --all", "does not support"),
        ("rm --keep-branch", "does not support"),
        ("close", "Rex has no session detach operation"),
        ("merge --squash", "needs interaction"),
        ("merge --into main~1", "existing local branch"),
    ],
)
def test_refusals(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    command: str,
    message: str,
):
    configure(mux_repo_path)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    request(node, ex_id, command)
    wait_message(node, ex_id, message)
    if command == "close":
        text = node_pane(node, ex_id)
        assert "select another session" in text and "workrex remove <handle>" in text
    assert len(local_records(mux_server)) == len(node.workspaces()) == 1
    assert node.sessions()


@pytest.mark.parametrize("command", ["rm", "merge"])
def test_dirty_refuses(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    command: str,
):
    configure(mux_repo_path)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    (node_worktree(node, "feat") / "dirty").write_text("uncommitted")
    request(node, ex_id, command)
    wait_message(node, ex_id, "has uncommitted changes")
    pane = node_pane(node, ex_id)
    expected = (
        "workrex rm --force"
        if command == "rm"
        else "workrex merge --ignore-uncommitted"
    )
    assert expected in pane
    assert "remote logs" not in pane
    assert local_records(mux_server) and node.workspaces()


def test_merge_conflict(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    configure(mux_repo_path)
    (mux_repo_path / "conflict").write_text("base\n")
    git(mux_repo_path, "add", "conflict")
    git(mux_repo_path, "commit", "-m", "base")
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    commit_on_node(node_worktree(node, "feat"), "conflict", "node\n")
    (mux_repo_path / "conflict").write_text("laptop\n")
    git(mux_repo_path, "commit", "-am", "laptop")
    request(node, ex_id, "merge")
    wait_message(node, ex_id, "merge conflict")
    assert local_records(mux_server) and node.workspaces()
    assert git(mux_repo_path, "show", "main:conflict") == "laptop"


def test_staged_changes_and_keep(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    configure(mux_repo_path)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    worktree = node_worktree(node, "feat")
    commit_on_node(worktree, "work.txt", "committed\n")
    (worktree / "staged").write_text("staged\n")
    git(worktree, "add", "staged")
    request(node, ex_id, "merge --keep")
    wait_message(node, ex_id, "needs interaction")
    assert git(mux_repo_path, "branch", "--show-current") == "main"
    request(node, ex_id, "merge --keep --ignore-uncommitted")
    wait_message(node, ex_id, "Done on the local machine")
    assert git(mux_repo_path, "show", "main:work.txt") == "committed"
    assert (worktree / "staged").read_text() == "staged\n"
    assert local_records(mux_server) and node.workspaces()


@pytest.mark.parametrize("hook", ["pre_merge", "post_create"])
def test_failed_hook_output_stays_on_laptop(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    hook: str,
):
    import re

    armed = node.root / "hook-armed"
    configure(
        mux_repo_path,
        **{
            hook: [
                f"if test -e {armed}; then "
                "echo LAPTOP-STDOUT; echo LAPTOP-STDERR >&2; exit 42; fi"
            ]
        },
    )
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    armed.touch()
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    commit_on_node(node_worktree(node, "feat"), "work.txt", "work\n")
    request(node, ex_id, "merge")
    wait_message(node, ex_id, "a local hook failed")
    pane = node_pane(node, ex_id)
    match = re.search(r"remote logs lo\s+(r[0-9a-f]{12})", pane)
    assert match, pane
    request_id = match.group(1)
    for private in ("LAPTOP-STDOUT", "LAPTOP-STDERR", str(mux_repo_path)):
        assert private not in pane
    output = wm(
        mux_server, workrex_exe_path, mux_repo_path, f"remote logs lo {request_id}"
    )
    assert "LAPTOP-STDOUT" in output.stdout
    assert "LAPTOP-STDERR" in output.stdout
    if hook == "pre_merge":
        assert "Pre-merge hook failed" in output.stdout
        assert "pane commands do not run" in output.stdout
    else:
        assert "Failed to run post-create command" in output.stdout
        assert "no pane commands" in output.stdout
    listing = wm(mux_server, workrex_exe_path, mux_repo_path, "remote logs lo")
    assert f"{request_id}.log" in listing.stdout
    assert local_records(mux_server) and node.workspaces()


def test_native_observer_owns_requests_until_the_last_workspace_is_removed(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    hooks = node.root / "remove-hooks"
    configure(mux_repo_path, pre_remove=[f"echo removed >> {hooks}"])
    for handle in ["one", "two"]:
        wm(mux_server, workrex_exe_path, mux_repo_path, f"add {handle} --remote lo")
        node.wait_for_agent(mux_server, handle, "AGENT-READY")
    assert poll_until(lambda: observer_owner_running(mux_server), timeout=10)
    for _ in range(3):
        wm(mux_server, workrex_exe_path, mux_repo_path, "open one", expect_fail=True)
    assert observer_owner_running(mux_server)
    request(node, node.workspace("one")["execution"]["ex_id"], "rm")
    assert poll_until(
        lambda: [record["handle"] for record in local_records(mux_server)] == ["two"],
        timeout=40,
    )
    assert observer_owner_running(mux_server)
    request(node, node.workspace("two")["execution"]["ex_id"], "rm")
    wait_removed(mux_server, node)
    assert poll_until(lambda: not observer_owner_running(mux_server), timeout=10)
    assert hooks.read_text().splitlines() == ["removed", "removed"]


def test_autonomous_merge_keeps_the_creators_effective_config(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    configure(mux_repo_path)
    hooks = node.root / "creator-merge-hooks"
    override = node.root / "creator.yaml"
    override.write_text(
        yaml.safe_dump(
            {
                "nerdfont": False,
                "panes": [{"command": "echo OVERRIDE-READY"}],
                "pre_merge": [f"echo merged >> {hooks}"],
            }
        )
    )
    wm(
        mux_server,
        workrex_exe_path,
        mux_repo_path,
        f"add feat --remote lo --config {shlex.quote(str(override))}",
    )
    node.wait_for_agent(mux_server, "feat", "OVERRIDE-READY")
    commit_on_node(node_worktree(node, "feat"), "work.txt", "creator config\n")
    execution = node.workspace("feat")["execution"]["ex_id"]
    override.unlink()
    global_config = mux_server.home_path / ".config/workrex/config.yaml"
    global_config.write_text("nerdfont: false\n")
    request(node, execution, "merge")
    wait_removed(mux_server, node)
    assert git(mux_repo_path, "show", "main:work.txt") == "creator config"
    assert hooks.read_text().splitlines() == ["merged"]
