"""Several remote agents on one node at once: two of one repository and one
of another, each with its own worktree, sessions and window."""

import json
import tempfile
from pathlib import Path

from ..rex_env import RexEnvironment


from ..conftest import (
    get_window_name,
    poll_until,
    setup_git_repo,
)
from .conftest import Node, commit_project, local_records, wm


def configure(repo: Path, marker: str) -> None:
    commit_project(repo, {"nerdfont": False, "panes": [{"command": f"echo {marker}"}]})


def test_agents_of_two_repos_share_a_node(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    first = mux_repo_path
    second = Path(tempfile.mkdtemp(prefix="second-repo-", dir=first.parent)) / "other"
    second.mkdir()
    setup_git_repo(second)
    configure(first, "FIRST-REPO")
    configure(second, "SECOND-REPO")

    for repo, branch in [(first, "alpha"), (first, "beta"), (second, "gamma")]:
        added = wm(mux_server, workrex_exe_path, repo, f"add {branch} --remote lo")
        assert "Successfully created remote workspace" in added.stdout, added.stderr
    for branch, marker in [
        ("alpha", "FIRST-REPO"),
        ("beta", "FIRST-REPO"),
        ("gamma", "SECOND-REPO"),
    ]:
        node.wait_for_agent(mux_server, branch, marker)

    workspaces = {ws["branch"]: ws for ws in node.workspaces()}
    assert sorted(workspaces) == ["alpha", "beta", "gamma"]
    assert workspaces["alpha"]["project_id"] == workspaces["beta"]["project_id"]
    assert workspaces["alpha"]["project_id"] != workspaces["gamma"]["project_id"]
    assert workspaces["alpha"]["repo_path"] == workspaces["beta"]["repo_path"]
    worktrees = {ws["worktree_path"] for ws in workspaces.values()}
    assert len(worktrees) == 3
    executions = [r["execution"]["ex_id"] for r in local_records(mux_server)]
    assert node.sessions() == sorted(f"{ex}-0" for ex in executions)

    # Each repository lists its own; --all lists every one.
    rows = json.loads(wm(mux_server, workrex_exe_path, first, "list --json").stdout)
    assert sorted(r["branch"] for r in rows if r.get("remote")) == ["alpha", "beta"]
    rows = json.loads(wm(mux_server, workrex_exe_path, second, "list --json").stdout)
    assert [r["branch"] for r in rows if r.get("remote")] == ["gamma"]

    # Removing one leaves the others running.
    wm(mux_server, workrex_exe_path, first, "remove --force beta")
    assert sorted(ws["branch"] for ws in node.workspaces()) == ["alpha", "gamma"]
    assert len(node.sessions()) == 2


def test_local_close_does_not_change_same_named_remote_session(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    first = mux_repo_path
    second = Path(tempfile.mkdtemp(prefix="second-repo-", dir=first.parent)) / "other"
    second.mkdir()
    setup_git_repo(second)
    configure(first, "REMOTE-PANE")
    configure(second, "LOCAL-PANE")
    wm(mux_server, workrex_exe_path, first, "add feat --remote lo")
    node.wait_for_agent(mux_server, "feat", "REMOTE-PANE")
    remote = node.workspace("feat")["execution"]["rex"]
    wm(mux_server, workrex_exe_path, second, "add feat")
    session = mux_server.session_view(get_window_name("feat"))
    [window] = session["windows"]
    assert poll_until(
        lambda: "LOCAL-PANE" in mux_server.capture_pane(window["window_id"]),
        timeout=20,
    )
    wm(mux_server, workrex_exe_path, second, "close feat")
    assert session["session_id"] not in {v["session_id"] for v in mux_server.views()}
    assert node.workspace("feat")["execution"]["rex"] == remote
    assert local_records(mux_server)[0]["handle"] == "feat"
    node.wait_for_agent(mux_server, "feat", "REMOTE-PANE")
