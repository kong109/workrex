"""Agent-spawned sibling workspaces keep independent native Rex identities."""

import time
from pathlib import Path

import yaml

from ..rex_env import RexEnvironment
from ..conftest import poll_until
from ..support.bin_name import hook_bin
from .conftest import Node, commit_project, git, local_records, wm, write_script

AGENT = """#!/bin/sh
# Applies commands written to agent-control in its worktree's git dir.
control="$(git rev-parse --absolute-git-dir)/agent-control"
echo FAKE-AGENT-READY
while :; do
  if [ -f "$control" ]; then
    command=$(cat "$control")
    rm -f "$control"
    case $command in
      spawn\\ *) workrex add ${command#spawn } </dev/null; echo "spawn-exit-$?" ;;
      *) workrex set-window-status "$command" </dev/null && echo "applied-$command" ;;
    esac
  fi
  sleep 0.1
done
"""


def setup_project(repo: Path) -> None:
    commit_project(
        repo, {"nerdfont": False, "panes": [{"command": "echo AGENT-READY"}]}
    )


def spawn(node: Node, ex_id: str, args: str) -> None:
    """Type `workrex add <args>` into the node session of `ex_id`, the way
    an agent without a sandbox would run it."""
    wrapper = node.root / "run" / "bin" / hook_bin()
    node.send(f"{ex_id}-0", f"{wrapper} add {args}")


def node_pane(node: Node, ex_id: str) -> str:
    return node.capture(f"{ex_id}-0")


def by_branch(node: Node, branch: str):
    found = [ws for ws in node.workspaces() if ws["branch"] == branch]
    return found[0] if found else None


def running(node: Node, branch: str) -> bool:
    ws = by_branch(node, branch)
    return bool(ws and ws.get("execution") and ws["execution"]["state"] == "running")


def setup_agent_project(repo: Path, agent: Path) -> None:
    agent.write_text(AGENT)
    agent.chmod(0o755)
    commit_project(
        repo,
        {"nerdfont": False, "agent": str(agent), "panes": [{"command": "<agent>"}]},
    )


def tell(worktree: Path, command: str) -> None:
    control = Path(git(worktree, "rev-parse", "--absolute-git-dir")) / "agent-control"
    tmp = control.with_suffix(".tmp")
    tmp.write_text(command)
    tmp.rename(control)


def test_forced_remove_of_a_sibling_stays_forgotten(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    repo = mux_repo_path
    setup_project(repo)
    wm(mux_server, workrex_exe_path, repo, "add feat --remote lo")
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    node.wait_for_agent(mux_server, "feat", "AGENT-READY")
    spawn(node, ex_id, "gone -b -p 'left behind'")
    assert poll_until(lambda: running(node, "gone"), timeout=20), node_pane(node, ex_id)
    wm(mux_server, workrex_exe_path, repo, "list")
    assert poll_until(
        lambda: any(r["branch"] == "gone" for r in local_records(mux_server)),
        timeout=20,
    )

    # Only this client's transport fails; the node and its sessions stay live.
    blocked = node.root / "blocked-ssh"
    write_script(
        mux_server.fake_bin_dir / "ssh",
        f"echo refused >> {blocked}\n"
        "echo 'ssh: connect to host localhost: Connection refused' >&2\nexit 255\n",
    )
    removed = wm(
        mux_server,
        workrex_exe_path,
        repo,
        "remove --force gone",
        extra_env={"WORKREX_REMOTE_LOOPBACK": "0"},
    )
    assert blocked.exists()
    assert "forgot remote workspace 'gone'" in removed.stderr, removed.stderr

    # Later looks at the node, which still runs it, leave it for gc.
    time.sleep(2)
    for _ in range(2):
        listing = wm(mux_server, workrex_exe_path, repo, "list")
        assert "adopted" not in listing.stderr, listing.stderr
    assert [r["branch"] for r in local_records(mux_server)] == ["feat"]
    assert running(node, "gone")
    gc = wm(mux_server, workrex_exe_path, repo, "remote gc lo --dry-run")
    assert "workspace 'gone'" in gc.stdout, gc.stdout + gc.stderr


def test_sibling_refuses_symlinks_from_the_parent(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    tmp_path: Path,
):
    repo = mux_repo_path
    setup_project(repo)
    wm(mux_server, workrex_exe_path, repo, "add feat --remote lo")
    [parent] = node.workspaces()
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    node.wait_for_agent(mux_server, "feat", "AGENT-READY")

    # The agent commits a symlink where the node writes the prompt file.
    outside = tmp_path / "outside"
    outside.mkdir()
    worktree = Path(parent["worktree_path"])
    (worktree / ".workrex").symlink_to(outside)
    git(worktree, "add", ".workrex")
    git(worktree, "commit", "-m", "point .workrex elsewhere")

    spawn(node, ex_id, "evil -p 'payload'")
    assert poll_until(
        lambda: "is a symlink in the checkout" in node_pane(node, ex_id), timeout=20
    ), node_pane(node, ex_id)
    assert list(outside.iterdir()) == []
    assert by_branch(node, "evil") is None
    assert [ws["branch"] for ws in node.workspaces()] == ["feat"]


def test_agent_spawns_independent_sibling_session(
    mux_server, workrex_exe_path, mux_repo_path, node, tmp_path
):
    repo = mux_repo_path
    setup_agent_project(repo, tmp_path / "fake-agent")
    project = yaml.safe_load((repo / ".workrex.yaml").read_text())
    project["remote_files"] = {"copy": ["copied-binary"]}
    commit_project(repo, project)
    with (repo / ".git/info/exclude").open("a") as excluded:
        excluded.write("\ncopied-binary\n")
    data = bytes(range(256)) * 1024
    (repo / "copied-binary").write_bytes(data)
    (repo / "copied-binary").chmod(0o751)
    wm(mux_server, workrex_exe_path, repo, "add feat --remote lo")
    parent = node.workspace("feat")
    node.wait_for_agent(mux_server, "feat", "FAKE-AGENT-READY")
    worktree = Path(parent["worktree_path"])
    (repo / "copied-binary").unlink()
    (worktree / "copied-binary").write_text("changed by parent")
    (worktree / "parent.txt").write_text("from parent\n")
    git(worktree, "add", "parent.txt")
    git(worktree, "commit", "-m", "parent work")
    tip = git(worktree, "rev-parse", "HEAD")
    tell(worktree, "spawn sib -b -p sibling-task")
    assert poll_until(lambda: running(node, "sib"), timeout=20), node_pane(
        node, parent["execution"]["ex_id"]
    )
    sibling = node.workspace("sib")
    assert sibling["origin"]["parent"] == parent["ws_id"]
    assert sibling["origin"]["background"] is True
    assert sibling["base_sha"] == tip
    assert sibling["isolation"] == parent["isolation"] == "none"
    child_tree = Path(sibling["worktree_path"])
    assert (child_tree / "copied-binary").read_bytes() == data
    assert (child_tree / "copied-binary").stat().st_mode & 0o777 == 0o751
    assert (child_tree / ".workrex/PROMPT-sib.md").read_text().strip() == "sibling-task"
    assert set(parent["execution"]["rex"]["sessions"].values()).isdisjoint(
        sibling["execution"]["rex"]["sessions"].values()
    )
    wm(mux_server, workrex_exe_path, repo, "list")
    assert poll_until(
        lambda: any(record["branch"] == "sib" for record in local_records(mux_server)),
        timeout=20,
    )
    node.wait_for_agent(mux_server, "sib", "FAKE-AGENT-READY")
    wm(mux_server, workrex_exe_path, repo, "remove --force sib")
    assert node.workspace("feat")["execution"]["rex"] == parent["execution"]["rex"]
    assert by_branch(node, "sib") is None


def test_sibling_spawned_without_an_app_is_adopted_by_observation(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    setup_project(mux_repo_path)
    wm(mux_server, workrex_exe_path, mux_repo_path, "add feat --remote lo")
    parent = node.workspace("feat")
    node.wait_for_agent(mux_server, "feat", "AGENT-READY")
    spawn(node, parent["execution"]["ex_id"], "later -b -p 'while away'")
    assert poll_until(lambda: running(node, "later"), timeout=20)
    assert poll_until(
        lambda: any(
            record["branch"] == "later" for record in local_records(mux_server)
        ),
        timeout=20,
    )
    listing = wm(mux_server, workrex_exe_path, mux_repo_path, "list")
    assert "later" in listing.stdout
    node.wait_for_agent(mux_server, "later", "AGENT-READY")
