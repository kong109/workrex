"""Native Rex lifecycle and workspace provisioning over the loopback transport."""

import json
import os
import subprocess
from pathlib import Path

import yaml

from ..rex_env import RexEnvironment
from ..conftest import get_worktree_path, poll_until
from .conftest import (
    LOOPBACK,
    Node,
    agent_dirs_env,
    commit_project,
    git,
    local_records,
    wm,
    write_script,
)


def setup_project(repo: Path) -> str:
    """Commit a project config locally (never pushed anywhere) and leave an
    ignored file for `remote_files.copy`; returns the commit."""
    project = {
        "nerdfont": False,
        "panes": [
            {"command": "echo AGENT-READY"},
            {"command": "echo SECOND-PANE", "split": "horizontal"},
        ],
        "remote_files": {"copy": [".env.local"]},
        "post_create": ["echo hooked > hook-ran.txt"],
    }
    (repo / ".workrex.yaml").write_text(yaml.dump(project))
    with open(repo / ".gitignore", "a") as gitignore:
        gitignore.write(".env.local\n")
    git(repo, "add", ".workrex.yaml", ".gitignore")
    git(repo, "commit", "-m", "configure remote panes")
    (repo / ".env.local").write_text("SECRET=fake\n")
    return git(repo, "rev-parse", "HEAD")


def test_add_resumes_interrupted_provisioning(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    """A failed `add --remote` is resumed by running it again: the same
    workspace continues where it stopped with the config as it is now, and
    finished hooks do not rerun."""
    repo = mux_repo_path
    allow = node.root / "allow-second-hook"
    project = {
        "nerdfont": False,
        "panes": [{"command": "echo AGENT-READY"}],
        "post_create": ["echo ran >> hooks.log", f"test -e {allow}"],
    }
    (repo / ".workrex.yaml").write_text(yaml.dump(project))
    git(repo, "add", ".workrex.yaml")
    git(repo, "commit", "-m", "configure hooks")

    failed = wm(
        mux_server, workrex_exe_path, repo, "add feat --remote lo", expect_fail=True
    )
    assert "test -e" in failed.stderr, failed.stderr
    assert "again to resume it" in failed.stderr
    [record] = local_records(mux_server)
    assert record["state"] == "provisioning"
    [incomplete] = node.workspaces()
    assert incomplete["ws_id"] == record["ws_id"]
    assert incomplete["execution"] is None and node.sessions() == []

    # The user fixes the hook and adds a pane before trying again.
    project["post_create"] = ["echo ran >> hooks.log", "true"]
    project["panes"].append({"command": "echo SECOND-PANE", "split": "horizontal"})
    (repo / ".workrex.yaml").write_text(yaml.dump(project))
    resumed = wm(mux_server, workrex_exe_path, repo, "add feat --remote lo")
    assert "Resuming remote workspace 'feat'" in resumed.stdout, resumed.stderr
    [workspace] = node.workspaces()
    assert workspace["ws_id"] == record["ws_id"]
    worktree = Path(workspace["worktree_path"])
    assert (worktree / "hooks.log").read_text() == "ran\n"
    [ready] = local_records(mux_server)
    assert ready["state"] == "ready"
    ex_id = ready["execution"]["ex_id"]
    assert node.sessions() == [f"{ex_id}-0"]
    assert len(ready["execution"]["rex"]["blocks"]) == 2
    node.wait_for_agent(mux_server, "feat", "AGENT-READY")
    node.wait_for_agent(mux_server, "feat", "SECOND-PANE")


def test_names_resolve_to_one_side(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    """A branch belongs to one workspace, and a local handle outranks a
    remote branch, so a name never reaches the wrong side."""
    repo = mux_repo_path
    setup_project(repo)
    wm(mux_server, workrex_exe_path, repo, "add x --name r1 --remote lo")

    for command in ["add x", "add x --name r2 --remote lo"]:
        refused = wm(mux_server, workrex_exe_path, repo, command, expect_fail=True)
        assert "Branch 'x' belongs to remote workspace 'r1' on 'lo'" in refused.stderr

    # A local worktree named `x` on another branch is reached by `x`.
    wm(mux_server, workrex_exe_path, repo, "add y --name x")
    local = get_worktree_path(repo, "x")
    path = wm(mux_server, workrex_exe_path, repo, "path x")
    assert Path(path.stdout.strip()).resolve() == local.resolve()
    wm(mux_server, workrex_exe_path, repo, "remove --force x")
    assert not local.exists()
    assert [ws["handle"] for ws in node.workspaces()] == ["r1"]
    assert [record["handle"] for record in local_records(mux_server)] == ["r1"]


def test_laptop_only_ignores_apply_on_the_node(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    """A copied file the laptop ignores through rules the node lacks stays
    ignored there, so an untouched workspace is clean and removes without
    --force."""
    repo = mux_repo_path
    project = {
        "nerdfont": False,
        "panes": [{"command": "echo AGENT-READY"}],
        "remote_files": {"copy": [".env.local"]},
    }
    (repo / ".workrex.yaml").write_text(yaml.dump(project))
    git(repo, "add", ".workrex.yaml")
    git(repo, "commit", "-m", "copy env")
    exclude = repo / ".git" / "info" / "exclude"
    exclude.parent.mkdir(parents=True, exist_ok=True)
    with open(exclude, "a") as rules:
        rules.write(".env.local\n")
    (repo / ".env.local").write_text("SECRET=fake\n")

    wm(mux_server, workrex_exe_path, repo, "add feat --remote lo")
    [workspace] = node.workspaces()
    worktree = Path(workspace["worktree_path"])
    assert (worktree / ".env.local").read_text() == "SECRET=fake\n"
    assert git(worktree, "status", "--porcelain") == ""

    removed = wm(mux_server, workrex_exe_path, repo, "remove feat", stdin_input="")
    assert "Removed remote workspace 'feat'" in removed.stdout, removed.stderr
    assert node.workspaces() == []


def test_host_only_workspaces_never_need_docker(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
    tmp_path: Path,
):
    """`isolation: none` starts and removes without docker, even where a
    docker CLI exists but cannot reach its daemon."""
    stub_dir = tmp_path / "bin"
    stub_dir.mkdir()
    stub = stub_dir / "docker"
    stub.write_text(
        "#!/bin/sh\necho 'Cannot connect to the Docker daemon' >&2\nexit 1\n"
    )
    stub.chmod(0o755)
    broken_docker = {"PATH": f"{stub_dir}:{os.environ['PATH']}"}
    repo = mux_repo_path
    setup_project(repo)

    added = wm(
        mux_server,
        workrex_exe_path,
        repo,
        "add feat --remote lo",
        extra_env=broken_docker,
    )
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    removed = wm(
        mux_server,
        workrex_exe_path,
        repo,
        "remove --force feat",
        extra_env=broken_docker,
    )
    assert "Removed remote workspace 'feat'" in removed.stdout, removed.stderr
    assert node.workspaces() == []


def test_setup_token_reaches_claude_panes_on_the_host(
    mux_server: RexEnvironment,
    workrex_exe_path: Path,
    mux_repo_path: Path,
    node: Node,
):
    """With `isolation: none` the node-wide setup token is in the session
    environment of a Claude workspace's panes."""
    token = "sk-ant-oat01-fake-token"
    (mux_server.fake_bin_dir / "claude").write_text(
        "#!/bin/sh\n"
        '[ "$1" = setup-token ] && { echo "fake setup-token flow"; exit 0; }\n'
        'echo "TOKEN-IN-PANE:${CLAUDE_CODE_OAUTH_TOKEN:-none}"\n'
        "exec sh\n"
    )
    (mux_server.fake_bin_dir / "claude").chmod(0o755)
    shell = write_script(
        mux_server.home_path / "login-shell",
        f'export PATH={mux_server.fake_bin_dir}:/usr/bin:/bin\nshift\nexec /bin/sh "$@"\n',
    )
    login = subprocess.run(
        [str(workrex_exe_path), "remote", "login", "lo", "claude", "--setup-token"],
        cwd=mux_repo_path,
        env={**mux_server.env, **LOOPBACK, "SHELL": str(shell)},
        input=f"{token}\n",
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert login.returncode == 0, login.stderr
    assert "fake setup-token flow" in login.stdout
    assert (node.root / "secrets" / "node.json").stat().st_mode & 0o777 == 0o600

    repo = mux_repo_path
    (repo / ".workrex.yaml").write_text(
        yaml.dump(
            {"nerdfont": False, "agent": "claude", "panes": [{"command": "<agent>"}]}
        )
    )
    git(repo, "add", ".workrex.yaml")
    git(repo, "commit", "-m", "claude panes")
    added = wm(mux_server, workrex_exe_path, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    node.wait_for_agent(mux_server, "feat", f"TOKEN-IN-PANE:{token}")
    assert list((node.root / "secrets").glob("*.env")) == []


def test_remote_lifecycle(
    mux_server, workrex_exe_path: Path, mux_repo_path: Path, node: Node
):
    repo = mux_repo_path
    head = setup_project(repo)
    added = wm(mux_server, workrex_exe_path, repo, "add feat --remote lo")
    assert "Successfully created remote workspace" in added.stdout, added.stderr
    [workspace] = node.workspaces()
    worktree = Path(workspace["worktree_path"])
    assert git(worktree, "rev-parse", "HEAD") == head
    assert git(worktree, "branch", "--show-current") == "feat"
    assert (worktree / ".env.local").read_text() == "SECRET=fake\n"
    assert (worktree / "hook-ran.txt").read_text().strip() == "hooked"
    assert not get_worktree_path(repo, "feat").exists()
    [record] = local_records(mux_server)
    ex_id = record["execution"]["ex_id"]
    placement = record["execution"]["rex"]
    assert set(placement["blocks"]) == {"0", "1"}
    assert len(set(placement["blocks"].values())) == 2
    assert len(set(placement["sessions"].values())) == 1
    assert node.sessions() == [f"{ex_id}-0"]
    node.wait_for_agent(mux_server, "feat", "AGENT-READY")
    node.wait_for_agent(mux_server, "feat", "SECOND-PANE")
    node.send(f"{ex_id}-0", "echo typed-$((40+2))")
    assert poll_until(lambda: "typed-42" in node.capture(f"{ex_id}-0"), timeout=10)
    rows = json.loads(wm(mux_server, workrex_exe_path, repo, "list --json").stdout)
    [row] = [row for row in rows if row.get("remote") == "lo"]
    assert row["handle"] == "feat" and row["is_open"] is True
    assert row["mode"] == "session"
    refused = wm(mux_server, workrex_exe_path, repo, "close feat", expect_fail=True)
    assert "select another Rex session" in refused.stderr, refused.stderr
    assert "workrex remove feat" in refused.stderr, refused.stderr
    assert node.sessions() == [f"{ex_id}-0"]
    opened = wm(mux_server, workrex_exe_path, repo, "open feat", expect_fail=True)
    assert "Rex.app" in opened.stderr
    assert local_records(mux_server)[0]["execution"]["ex_id"] == ex_id
    assert "typed-42" in node.capture(f"{ex_id}-0")
    removed = wm(mux_server, workrex_exe_path, repo, "remove --force feat")
    assert "Removed remote workspace 'feat'" in removed.stdout
    assert not worktree.exists()
    assert node.workspaces() == [] and node.sessions() == []
    assert local_records(mux_server) == []
    assert git(Path(workspace["repo_path"]), "branch", "--list", "feat") == ""


def test_startup_output_is_preserved_in_the_native_block(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    commit_project(
        mux_repo_path,
        {"panes": [{"command": "echo ran >> command-runs; echo FIRST-PANE-READY"}]},
    )
    wm(mux_server, workrex_exe_path, mux_repo_path, "add first --remote lo")
    node.wait_for_agent(mux_server, "first", "FIRST-PANE-READY")
    workspace = node.workspace("first")
    assert (Path(workspace["worktree_path"]) / "command-runs").read_text() == "ran\n"
    before = workspace["execution"]["rex"]["blocks"]
    wm(mux_server, workrex_exe_path, mux_repo_path, "open first", expect_fail=True)
    assert node.workspace("first")["execution"]["rex"]["blocks"] == before
    assert (Path(workspace["worktree_path"]) / "command-runs").read_text() == "ran\n"


def test_remote_panes_advertise_truecolor(
    mux_server, workrex_exe_path, mux_repo_path, node
):
    commit_project(
        mux_repo_path,
        {
            "panes": [
                {"command": "printf 'COLOR=%s\\n' \"$COLORTERM\""},
                {
                    "command": "printf 'SECOND=%s\\n' \"$COLORTERM\"",
                    "split": "horizontal",
                },
            ]
        },
    )
    wm(
        mux_server,
        workrex_exe_path,
        mux_repo_path,
        "add feat --remote lo",
        extra_env={"COLORTERM": "256color"},
    )
    node.wait_for_agent(mux_server, "feat", "COLOR=truecolor")
    node.wait_for_agent(mux_server, "feat", "SECOND=truecolor")


def test_add_wait_tracks_the_native_remote_session(
    mux_server, workrex_exe_path, mux_repo_path, node, tmp_path
):
    commit_project(mux_repo_path, {"panes": [{"command": "echo WAIT-READY"}]})
    log_path = tmp_path / "add-wait.log"
    with log_path.open("w") as log:
        process = subprocess.Popen(
            [str(workrex_exe_path), "add", "feat", "--remote", "lo", "--wait"],
            cwd=mux_repo_path,
            env={
                **mux_server.env,
                **LOOPBACK,
                **agent_dirs_env(mux_server.home_path),
                "REX_SERVER": mux_server.server_url,
                "REX_SESSION": mux_server.session_id,
                "REX_BLOCK": mux_server.runner_block_id,
            },
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        try:
            assert poll_until(
                lambda: any(w.get("execution") for w in node.workspaces()),
                timeout=20,
            ), log_path.read_text()
            node.wait_for_agent(mux_server, "feat", "WAIT-READY")
            assert poll_until(
                lambda: "Waiting for remote Rex target 'feat' on 'lo' to close..."
                in log_path.read_text(),
                timeout=10,
            ), log_path.read_text()
            assert not poll_until(lambda: process.poll() is not None, timeout=1), (
                log_path.read_text()
            )
            wm(mux_server, workrex_exe_path, mux_repo_path, "remove feat")
            assert process.wait(timeout=20) == 0, log_path.read_text()
            assert node.workspaces() == [] and local_records(mux_server) == []
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
