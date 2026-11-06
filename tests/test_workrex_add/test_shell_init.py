"""Tests for shell initialization and login shell behavior."""

from ..conftest import ShellCommands, poll_until, wait_for_file, write_workrex_config
from ..rex_env import RexEnvironment
from .conftest import add_branch_and_get_worktree


class TestLoginShell:
    """Tests that workrex starts shells as login shells."""

    def test_panes_start_as_login_shells(
        self,
        mux_server: RexEnvironment,
        workrex_exe_path,
        repo_path,
        shell_cmd: ShellCommands,
    ):
        """
        Verifies that panes are started as login shells by checking if
        the shell's RC file is sourced.
        """
        env = mux_server
        branch_name = "feature-login-shell"
        marker_file = env.home_path / "profile_loaded"

        # 1. Configure the shell
        env.configure_default_shell(shell_cmd.path)

        # 2. Append marker creation to RC file
        # This is only executed if the shell starts properly
        rc_path = env.home_path / shell_cmd.rc_filename
        with rc_path.open("a") as f:
            f.write(f"touch {marker_file}\n")

        # 3. Create workrex config with a command
        # A command is required to trigger the wrapper logic in setup_panes
        write_workrex_config(repo_path, panes=[{"command": "echo 'starting pane'"}])

        # 4. Run workrex add
        add_branch_and_get_worktree(env, workrex_exe_path, repo_path, branch_name)

        # 5. Wait for marker file
        # This confirms that the shell executed the RC file
        wait_for_file(env, marker_file, timeout=5.0)

    def test_split_panes_start_as_login_shells(
        self,
        mux_server: RexEnvironment,
        workrex_exe_path,
        repo_path,
        shell_cmd: ShellCommands,
    ):
        """
        Verifies that split panes are also started as login shells.
        """
        env = mux_server
        branch_name = "feature-split-login"
        log_file = env.home_path / "profile_log"

        # 1. Configure the shell
        env.configure_default_shell(shell_cmd.path)

        # 2. Append log-writing to RC file
        rc_path = env.home_path / shell_cmd.rc_filename
        with rc_path.open("a") as f:
            f.write(shell_cmd.append_to_file("loaded", str(log_file)) + "\n")

        # 3. Create workrex config with two panes (one initial, one split)
        write_workrex_config(
            repo_path,
            panes=[
                {"command": "echo pane1"},
                {"split": "horizontal", "command": "echo pane2"},
            ],
        )

        # 4. Run workrex add
        add_branch_and_get_worktree(env, workrex_exe_path, repo_path, branch_name)

        # 5. Wait for log file to have 2 lines (one for each pane)
        def check_log_lines():
            if not log_file.exists():
                return False
            content = log_file.read_text()
            return content.strip().count("loaded") >= 2

        assert poll_until(check_log_lines, timeout=5.0), (
            f"Expected 2 login shells, log content:\n"
            f"{log_file.read_text() if log_file.exists() else 'File not found'}"
        )
