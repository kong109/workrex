import json
import os
import re
import shlex
import shutil
import subprocess
import tempfile
import time
import unicodedata
from abc import ABC, abstractmethod
from collections.abc import Callable, Generator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

from .support.bin_name import BIN
from .support.executable import SCRIPT_RUNNER, install_script

# =============================================================================
# Shell Testing Configuration
# =============================================================================

# Shell names to test - paths are discovered dynamically via shutil.which()
SHELL_NAMES = ["bash", "zsh", "fish", "nu"]


@dataclass
class ShellCommands:
    """Shell-specific command generation for multi-shell testing."""

    path: str

    @property
    def name(self) -> str:
        """Return shell name (e.g., 'zsh', 'bash', 'fish', 'nu')."""
        return Path(self.path).name

    @property
    def rc_filename(self) -> str:
        """Return the RC file path relative to HOME for this shell.

        Note: Bash uses .bash_profile because Rex starts login shells (-l),
        which read .bash_profile, not .bashrc.
        """
        return {
            "bash": ".bash_profile",
            "zsh": ".zshrc",
            "fish": ".config/fish/config.fish",
            "nu": ".config/nushell/config.nu",
        }[self.name]

    def set_env(self, var: str, value: str) -> str:
        """Generate shell-specific environment variable export statement.

        Note: Do not use this for PATH - use prepend_path() instead.
        """
        match self.name:
            case "fish":
                return f"set -gx {var} '{value}'"
            case "nu":
                return f"$env.{var} = '{value}'"
            case _:
                return f"export {var}='{value}'"

    def env_ref(self, var: str) -> str:
        """Return shell-specific syntax for referencing an environment variable."""
        match self.name:
            case "nu":
                return f"$env.{var}"
            case _:
                return f"${var}"

    def prepend_path(self, dir_path: str) -> str:
        """Generate shell-specific command to prepend a directory to PATH.

        Fish and nushell treat PATH as a list, not a colon-separated string,
        so they need special handling.
        """
        match self.name:
            case "fish":
                return f"set -gx PATH '{dir_path}' $PATH"
            case "nu":
                return f"$env.PATH = ($env.PATH | prepend '{dir_path}')"
            case _:
                return f"export PATH='{dir_path}':$PATH"

    def alias(self, name: str, command: str) -> str:
        """Generate shell-specific alias definition.

        Note: Nushell aliases use `alias name = cmd` syntax. The ^ prefix
        forces nushell to call the external command rather than recursing.
        """
        match self.name:
            case "fish":
                return f"alias {name} '{command}'"
            case "nu":
                # Use ^ to call external command, avoiding infinite recursion
                return f"alias {name} = ^{command}"
            case _:
                return f"alias {name}='{command}'"

    def append_to_file(self, text: str, file_path: str) -> str:
        """Generate shell-specific command to append text to a file."""
        match self.name:
            case "nu":
                return f'"{text}" | save --append {file_path}'
            case _:
                return f"echo '{text}' >> {file_path}"


def get_shells_to_test() -> list[str]:
    """Return list of shell paths to test based on environment variables.

    Environment variables:
        TEST_SHELL: Test a specific shell only (e.g., "fish", "nu", "bash", "zsh")

    By default, tests run against all installed shells (bash, zsh, fish, nu).
    Uses shutil.which() to discover actual shell paths rather than hardcoding,
    ensuring portability across different systems (Linux, macOS, Homebrew, etc.).
    """
    # Test a specific shell
    if specific_shell := os.environ.get("TEST_SHELL"):
        path = shutil.which(specific_shell)
        if path:
            return [path]
        raise ValueError(f"Shell '{specific_shell}' not found")

    # Default: test all installed shells
    shells = [p for p in (shutil.which(name) for name in SHELL_NAMES) if p]
    return shells if shells else ["/bin/sh"]


# =============================================================================
# Multiplexer Environment Abstraction
# =============================================================================


# Variables an agent sets that workrex reads: where the agent keeps its
# config, and the session it runs.
AGENT_VARIABLES = (
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_JOB_DIR",
    "CODEX_HOME",
    "CODEX_CONFIG_DIR",
    "CODEX_THREAD_ID",
    "COPILOT_HOME",
    "COPILOT_CONFIG_DIR",
    "GEMINI_CLI_HOME",
    "GEMINI_CONFIG_DIR",
    "GROK_HOME",
    "MUSE_SESSION_ID",
    "OPENCODE_CONFIG",
    "OPENCODE_CONFIG_DIR",
    "PI_CODING_AGENT_DIR",
    "PI_CONFIG_DIR",
)


class MuxEnvironment(ABC):
    """
    Shared environment and process helpers for isolated Rex tests.

    Provides a common interface for running tests against different
    isolated Rex sessions, windows, and terminal blocks.
    """

    def __init__(self, tmp_path: Path):
        self.tmp_path = tmp_path
        self._scripts_dir: Path | None = None  # Lazily created by get_scripts_dir()

        # Create isolated home directory
        self.home_path = self.tmp_path / "test_home"
        self.home_path.mkdir()

        # Create a shared fake-bin directory for test agent scripts.
        # Write base RC files for every supported shell that prepend this
        # directory to PATH.  This survives macOS path_helper reordering
        # in login shells, which would otherwise push the fake dir to the
        # end of PATH and let a real installed agent win.
        self.fake_bin_dir = self.tmp_path / "fake-bin"
        self.fake_bin_dir.mkdir()
        for shell_name in SHELL_NAMES:
            sc = ShellCommands(shell_name)
            rc_path = self.home_path / sc.rc_filename
            rc_path.parent.mkdir(parents=True, exist_ok=True)
            rc_path.write_text(sc.prepend_path(str(self.fake_bin_dir)) + "\n")

        # Base environment setup
        self.env = os.environ.copy()
        self.env["PATH"] = f"{self.fake_bin_dir}:{self.env.get('PATH', '')}"
        self.env["TMPDIR"] = str(self.tmp_path)
        self.env["HOME"] = str(self.home_path)
        # Explicitly set XDG_STATE_HOME to ensure state files are isolated
        # (config uses $HOME/.config/ directly, so HOME isolation handles that)
        self.env["XDG_STATE_HOME"] = str(self.home_path / ".local" / "state")
        self.env["XDG_CONFIG_HOME"] = str(self.home_path / ".config")
        self.env["XDG_CACHE_HOME"] = str(self.home_path / ".cache")
        self.env["XDG_DATA_HOME"] = str(self.home_path / ".local" / "share")
        # An agent's own directory variable would send setup outside HOME,
        # and the agent's session variables would change how hooks behave.
        for name in AGENT_VARIABLES:
            self.env.pop(name, None)

        # Create fake git editor
        fake_editor_script = self.home_path / "fake_git_editor.sh"
        fake_editor_script.write_text(
            "#!/bin/sh\n"
            'if ! grep -q "^[^#]" "$1" 2>/dev/null; then\n'
            '  echo "Test commit" > "$1"\n'
            "fi\n"
        )
        self.env["GIT_EDITOR"] = f"/bin/sh {shlex.quote(str(fake_editor_script))}"

    def install_script(self, path: Path, body: str) -> Path:
        """Install a PATH-discoverable test double with a shared entry point."""
        return install_script(path, body)

    @property
    @abstractmethod
    def backend_name(self) -> str:
        """Return the Rex backend name."""

    @abstractmethod
    def start_server(self) -> None:
        """Start the multiplexer server with an initial session."""

    @abstractmethod
    def stop_server(self) -> None:
        """Stop the multiplexer server and clean up resources."""

    def run_command(self, cmd: list[str], check: bool = True, cwd: Path | None = None):
        """Run a generic command within the isolated environment."""
        working_dir = cwd if cwd is not None else self.tmp_path
        result = subprocess.run(
            cmd,
            cwd=working_dir,
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )
        if check and result.returncode != 0:
            raise subprocess.CalledProcessError(
                result.returncode, cmd, result.stdout, result.stderr
            )
        return result

    @abstractmethod
    def mux_command(
        self, args: list[str], check: bool = True
    ) -> subprocess.CompletedProcess[str]:
        """
        Run a backend-specific command.

        Run the native Rex CLI against the test server.
        """

    @abstractmethod
    def list_windows(self) -> list[str]:
        """Return a list of all window/tab names."""

    @abstractmethod
    def list_window_ids(self) -> list[str]:
        """Return stable window/tab identities, independent of automatic titles."""

    @abstractmethod
    def capture_pane(self, window_name: str) -> str | None:
        """Capture the content of a pane in the specified window."""

    @abstractmethod
    def send_keys(self, target: str, text: str, enter: bool = True) -> None:
        """
        Send text to a pane.

        Args:
            target: Window/pane identifier
            text: Text to send
            enter: Whether to send Enter key after text
        """

    @abstractmethod
    def run_shell_background(self, script: str) -> None:
        """
        Run a shell script in the background.

        Used for commands that may kill their own window (like merge/remove).
        """

    @abstractmethod
    def set_session_env(self, key: str, value: str) -> None:
        """Set an environment variable in the multiplexer session."""

    @abstractmethod
    def kill_window(self, window_name: str) -> None:
        """Kill/close a specific window by name."""

    @abstractmethod
    def get_current_window(self) -> str | None:
        """Get the name of the currently focused window."""

    @abstractmethod
    def select_window(self, window_name: str) -> None:
        """Switch focus to a specific window by name."""

    @abstractmethod
    def new_window(self, name: str | None = None) -> None:
        """Create a new window/tab with optional name."""

    def list_sessions(self) -> list[str]:
        raise NotImplementedError(f"{self.backend_name} has no sessions")

    def new_session(self, name: str) -> None:
        raise NotImplementedError(f"{self.backend_name} has no sessions")

    def list_windows_in_session(self, session: str) -> list[str]:
        raise NotImplementedError(f"{self.backend_name} has no sessions")

    def session_pane_path(self, session: str) -> str:
        raise NotImplementedError(f"{self.backend_name} has no sessions")

    @abstractmethod
    def configure_default_shell(self, shell: str) -> None:
        """Configure the default shell for new panes.

        Set the shell used in new Rex terminal blocks.
        """


def skip_if_backend_unavailable(backend: str):
    """Skip live terminal checks when the Rex executable is absent."""
    from .rex_env import rex_binary

    if backend != "rex":
        raise ValueError("Workrex tests require the Rex backend")
    if rex_binary() is None:
        pytest.skip("rex not installed")


MuxEnv = MuxEnvironment
DEFAULT_WINDOW_PREFIX = "wr-"

# =============================================================================
# Shared Assertion Helpers
# =============================================================================


def assert_window_exists(env: MuxEnvironment, window_name: str) -> None:
    """Ensure a window/tab with the provided name exists."""
    existing_windows = env.list_windows()
    assert window_name in existing_windows, (
        f"Window {window_name!r} not found. Existing: {existing_windows!r}"
    )


def assert_session_exists(env: "MuxEnvironment", session_name: str) -> None:
    """Ensure a session with the provided name exists."""
    existing_sessions = env.list_sessions()
    assert session_name in existing_sessions, (
        f"Session {session_name!r} not found. Existing: {existing_sessions!r}"
    )


def assert_session_not_exists(env: "MuxEnvironment", session_name: str) -> None:
    """Ensure a session with the provided name does NOT exist."""
    existing_sessions = env.list_sessions()
    assert session_name not in existing_sessions, (
        f"Session {session_name!r} should not exist but was found. Existing: {existing_sessions!r}"
    )


def assert_window_not_exists(env: "MuxEnvironment", window_name: str) -> None:
    """Ensure a window with the provided name does NOT exist."""
    existing_windows = env.list_windows()
    assert window_name not in existing_windows, (
        f"Window {window_name!r} should not exist but was found. Existing: {existing_windows!r}"
    )


def assert_copied_file(
    worktree_path: Path, relative_path: str, expected_text: str | None = None
) -> Path:
    """Assert that a copied file exists in the worktree and is not a symlink."""
    file_path = worktree_path / relative_path
    assert file_path.exists(), f"Expected copied file {relative_path} to exist"
    assert not file_path.is_symlink(), (
        f"Expected {relative_path} to be a regular file, but found a symlink"
    )
    if expected_text is not None:
        assert file_path.read_text() == expected_text
    return file_path


def assert_symlink_to(worktree_path: Path, relative_path: str) -> Path:
    """Assert that a symlink exists in the worktree and return the path."""
    symlink_path = worktree_path / relative_path
    assert symlink_path.exists(), f"Expected symlink {relative_path} to exist"
    assert symlink_path.is_symlink(), f"Expected {relative_path} to be a symlink"
    return symlink_path


# =============================================================================
# Polling & Wait Helpers
# =============================================================================


def wait_for_window_ready(
    env: MuxEnvironment, window_name: str, timeout: float = 3.0
) -> None:
    """Poll until a window exists and its pane has visible content (shell prompt)."""

    def _ready() -> bool:
        if window_name not in env.list_windows():
            return False
        content = env.capture_pane(window_name)
        return content is not None and content.strip() != ""

    if not poll_until(_ready, timeout=timeout):
        assert False, f"Window {window_name!r} not ready within {timeout}s"


def wait_for_pane_output(
    env: MuxEnvironment, window_name: str, text: str, timeout: float = 2.0
) -> None:
    """Poll until the specified text appears in the pane."""

    final_content = f"Pane for window '{window_name}' was not captured."

    def _has_output() -> bool:
        nonlocal final_content
        content = env.capture_pane(window_name)
        if content is not None:
            final_content = content
            return text in final_content
        final_content = f"Error capturing pane for window '{window_name}'"
        return False

    if not poll_until(_has_output, timeout=timeout):
        assert False, (
            f"Expected output {text!r} not found in window {window_name!r} within {timeout}s.\n"
            f"--- FINAL PANE CONTENT ---\n"
            f"{final_content}\n"
            f"--------------------------"
        )


def wait_for_file(
    env: MuxEnvironment,
    file_path: Path,
    timeout: float = 5.0,
    *,
    window_name: str | None = None,
    worktree_path: Path | None = None,
    debug_log_path: Path | None = None,
) -> None:
    """
    Poll for a file to exist. On timeout, fail with diagnostics about panes, worktrees, and logs.
    """

    def _file_exists() -> bool:
        return file_path.exists()

    if poll_until(_file_exists, timeout=timeout):
        return

    diagnostics: list[str] = [f"Target file: {file_path}"]

    if worktree_path is not None:
        diagnostics.append(f"Worktree path: {worktree_path}")
        if worktree_path.exists():
            try:
                files = sorted(p.name for p in worktree_path.iterdir())
                diagnostics.append(f"Worktree files: {files}")
            except OSError as exc:  # pragma: no cover - best effort diagnostics
                diagnostics.append(f"Error listing worktree files: {exc}")
        else:
            diagnostics.append("Worktree directory not found.")

    if debug_log_path is not None:
        if debug_log_path.exists():
            diagnostics.append(
                f"Debug log '{debug_log_path.name}':\n{debug_log_path.read_text()}"
            )
        else:
            diagnostics.append(f"Debug log '{debug_log_path.name}' not found.")

    if window_name is not None:
        pane_content = env.capture_pane(window_name)
        if pane_content is None:
            pane_content = f"Could not capture pane for window '{window_name}'."
        diagnostics.append(f"Pane '{window_name}' content:\n{pane_content}")

    diag_str = "\n".join(diagnostics)
    assert False, (
        f"File not found after {timeout}s: {file_path}\n\n"
        f"-- Diagnostics --\n{diag_str}\n-----------------"
    )


# =============================================================================
# Path & Naming Helpers
# =============================================================================


def prompt_file_for_branch(worktree_path: Path, branch_name: str) -> Path:
    """Return the path to the prompt file for the given branch.

    Prompt files are now stored in <worktree>/.workrex/PROMPT-<sanitized-branch>.md
    Branch names with slashes are sanitized to dashes.
    """
    sanitized_branch = branch_name.replace("/", "-")
    return worktree_path / ".workrex" / f"PROMPT-{sanitized_branch}.md"


def assert_prompt_file_contents(
    env: MuxEnvironment,
    branch_name: str,
    expected_text: str,
    worktree_path: Path | None = None,
) -> None:
    """Assert that a prompt file exists for the branch and matches the expected text."""
    if worktree_path is None:
        raise ValueError(
            "worktree_path is required - prompt files are now in <worktree>/.workrex/"
        )
    prompt_file = prompt_file_for_branch(worktree_path, branch_name)
    assert prompt_file.exists(), f"Prompt file not found at {prompt_file}"
    actual_text = prompt_file.read_text()
    assert actual_text == expected_text, (
        f"Content mismatch for prompt file: {prompt_file}"
    )


def file_for_commit(worktree_path: Path, commit_message: str) -> Path:
    """Return the expected file path generated by create_commit for a message."""
    sanitized = commit_message.replace(" ", "_").replace(":", "")
    return worktree_path / f"file_for_{sanitized}.txt"


# =============================================================================
# RepoBuilder - Declarative Git Repository Setup
# =============================================================================


@dataclass
class RepoBuilder:
    """Builder pattern for setting up git repositories declaratively in tests."""

    env: MuxEnvironment
    path: Path
    _files_to_add: list[str] = field(default_factory=list)

    def with_file(self, relative_path: str, content: str) -> "RepoBuilder":
        """Create a file with the given content."""
        file_path = self.path / relative_path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_text(content)
        self._files_to_add.append(relative_path)
        return self

    def with_files(self, files: dict[str, str]) -> "RepoBuilder":
        """Create multiple files from a dict of path -> content."""
        for rel_path, content in files.items():
            self.with_file(rel_path, content)
        return self

    def with_dir(self, relative_path: str) -> "RepoBuilder":
        """Create an empty directory."""
        dir_path = self.path / relative_path
        dir_path.mkdir(parents=True, exist_ok=True)
        return self

    def with_executable(self, relative_path: str, content: str) -> "RepoBuilder":
        """Create an executable file."""
        file_path = self.path / relative_path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_text(content)
        file_path.chmod(0o755)
        self._files_to_add.append(relative_path)
        return self

    def commit(self, message: str = "Update files") -> "RepoBuilder":
        """Stage all pending files and commit."""
        if self._files_to_add:
            self.env.run_command(["git", "add"] + self._files_to_add, cwd=self.path)
            self._files_to_add.clear()
        else:
            self.env.run_command(["git", "add", "."], cwd=self.path)
        self.env.run_command(["git", "commit", "-m", message], cwd=self.path)
        return self

    def add_to_gitignore(self, patterns: list[str]) -> "RepoBuilder":
        """Append patterns to .gitignore."""
        gitignore_path = self.path / ".gitignore"
        with gitignore_path.open("a") as f:
            for pattern in patterns:
                f.write(f"{pattern}\n")
        return self


@pytest.fixture
def repo_builder(mux_server: MuxEnvironment, repo_path: Path) -> RepoBuilder:
    """Provides a RepoBuilder for declarative git setup in tests."""
    return RepoBuilder(env=mux_server, path=repo_path)


# =============================================================================
# Fake Agent Installation
# =============================================================================


@dataclass
class FakeAgentInstaller:
    """Factory for installing fake agent commands in tests.

    Scripts are placed in MuxEnvironment.fake_bin_dir, which is already
    on PATH via both the subprocess environment and shell RC files.
    """

    env: MuxEnvironment

    @property
    def bin_dir(self) -> Path:
        return self.env.fake_bin_dir

    def install(self, name: str, script_body: str) -> Path:
        """Creates a fake agent command; the bin dir is already on PATH."""
        return self.env.install_script(self.bin_dir / name, script_body)


@pytest.fixture(scope="session")
def script_runner() -> Path:
    """All test doubles share the repository runner's executable inode."""
    return SCRIPT_RUNNER


@pytest.fixture
def fake_agent_installer(mux_server: MuxEnvironment) -> FakeAgentInstaller:
    """Provides a factory for installing fake agent commands."""
    return FakeAgentInstaller(env=mux_server)


def slugify(text: str) -> str:
    """
    Convert text to a slug, matching the behavior of the Rust `slug` crate.

    - Converts to lowercase
    - Replaces non-alphanumeric characters with dashes
    - Removes leading/trailing dashes
    - Collapses multiple dashes to single dash
    """
    # Normalize unicode characters (e.g., é -> e)
    text = unicodedata.normalize("NFKD", text)
    text = text.encode("ascii", "ignore").decode("ascii")

    # Convert to lowercase
    text = text.lower()

    # Replace non-alphanumeric characters with dashes
    text = re.sub(r"[^a-z0-9]+", "-", text)

    # Remove leading/trailing dashes and collapse multiple dashes
    text = re.sub(r"-+", "-", text)
    text = text.strip("-")

    return text


def pytest_addoption(parser):
    parser.addoption(
        "--backend",
        action="store",
        choices=["rex"],
        default="rex",
        help="Terminal backend, always Rex",
    )


def get_test_backends(config) -> list[str]:
    """Workrex exercises only its Rex backend."""
    requested = os.environ.get("WORKREX_TEST_BACKEND", "rex")
    if requested != "rex":
        raise ValueError("WORKREX_TEST_BACKEND must be rex")
    return ["rex"]


@pytest.fixture
def mux_server(tmp_path: Path) -> Generator[MuxEnvironment, None, None]:
    """A dedicated Rex server, stopped only through its owned process handle."""

    from .rex_env import RexEnvironment

    skip_if_backend_unavailable("rex")
    test_env = RexEnvironment(tmp_path)
    test_env.start_server()
    try:
        yield test_env
    finally:
        test_env.stop_server()
        if test_env._scripts_dir is not None:
            shutil.rmtree(test_env._scripts_dir, ignore_errors=True)


@pytest.fixture(params=get_shells_to_test(), ids=lambda s: Path(s).name)
def shell_cmd(request) -> ShellCommands:
    """
    Fixture providing shell-specific command helpers.

    By default, tests run with zsh only. To test all available shells:
        TEST_ALL_SHELLS=1 pytest tests/

    Tests using this fixture will run once per enabled shell.
    """
    shell_path = request.param
    if not os.path.exists(shell_path):
        pytest.skip(f"Shell {shell_path} not available")
    return ShellCommands(shell_path)


def setup_git_repo(path: Path, env_vars: dict | None = None):
    """Initializes a git repository in the given path with an initial commit."""
    subprocess.run(
        ["git", "init", "-b", "main"],
        cwd=path,
        check=True,
        capture_output=True,
        env=env_vars,
    )
    # Configure git user for commits
    subprocess.run(
        ["git", "config", "user.name", "Test User"],
        cwd=path,
        check=True,
        capture_output=True,
        env=env_vars,
    )
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"],
        cwd=path,
        check=True,
        capture_output=True,
        env=env_vars,
    )
    # Ignore test_home directory and test output files to prevent uncommitted changes
    gitignore_path = path / ".gitignore"
    gitignore_path.write_text(
        "test_home/\nworkrex_*.txt\n"  # Test helper output files
    )
    subprocess.run(
        ["git", "add", ".gitignore"],
        cwd=path,
        check=True,
        capture_output=True,
        env=env_vars,
    )
    subprocess.run(
        ["git", "commit", "--allow-empty", "-m", "Initial commit"],
        cwd=path,
        check=True,
        capture_output=True,
        env=env_vars,
    )


@pytest.fixture(scope="session")
def _template_git_repo(tmp_path_factory) -> Path:
    """Create a pristine git repository once per test session.

    This avoids running 5 git subprocess calls (init, config x2, add, commit)
    per test. Tests copy .git/ and .gitignore from this template instead.
    """
    path = tmp_path_factory.mktemp("template_repo")
    setup_git_repo(path)
    return path


@pytest.fixture
def repo_path(mux_server: MuxEnvironment, _template_git_repo: Path) -> Path:
    """Initializes a git repo in the test env and returns its path.

    Copies from a session-scoped template repo instead of running git init
    per test. The git config (user.name, user.email) is stored in .git/config
    and survives the copy.

    The repository is created inside the isolated Rex server environment.
    """
    path = mux_server.tmp_path
    shutil.copytree(_template_git_repo / ".git", path / ".git")
    shutil.copy(_template_git_repo / ".gitignore", path / ".gitignore")
    return path


# Backward compatibility alias - tests using mux_repo_path will continue to work
@pytest.fixture
def mux_repo_path(repo_path: Path) -> Path:
    """Alias for repo_path - for backward compatibility."""
    return repo_path


@pytest.fixture
def remote_repo_path(mux_server: MuxEnvironment) -> Path:
    """Creates a bare git repo to act as a remote.

    The fixture uses an isolated Rex server.
    """
    parent = mux_server.tmp_path.parent
    remote_path = Path(tempfile.mkdtemp(prefix="remote_repo_", dir=parent))
    subprocess.run(
        ["git", "init", "--bare"],
        cwd=remote_path,
        check=True,
        capture_output=True,
    )
    return remote_path


# Backward compatibility alias
@pytest.fixture
def mux_remote_repo_path(remote_repo_path: Path) -> Path:
    """Alias for remote_repo_path - for backward compatibility."""
    return remote_repo_path


def poll_until(
    condition: Callable[[], bool],
    timeout: float = 5.0,
    poll_interval: float = 0.1,
) -> bool:
    """
    Poll until a condition is met or timeout is reached.

    Uses adaptive backoff: checks immediately, then ramps up the interval.
    The poll_interval parameter is kept for API compatibility but the adaptive
    schedule is always used.

    Args:
        condition: A callable that returns True when the condition is met
        timeout: Maximum time to wait in seconds
        poll_interval: Time to wait between checks in seconds (ignored, adaptive used)

    Returns:
        True if condition was met, False if timeout was reached
    """
    end = time.monotonic() + timeout
    intervals = [0.0, 0.01, 0.02, 0.05, 0.1]
    i = 0
    while time.monotonic() < end:
        if condition():
            return True
        time.sleep(intervals[min(i, len(intervals) - 1)])
        i += 1
    return False


def poll_until_file_has_content(file_path: Path, timeout: float = 5.0) -> bool:
    """
    Poll until a file exists and has non-empty content.

    This avoids race conditions where a file is created but not yet written to.
    Shell redirection like `echo $? > file` may create an empty file before
    writing the actual content.

    Args:
        file_path: Path to the file to check
        timeout: Maximum time to wait in seconds

    Returns:
        True if file exists with content, False if timeout was reached
    """

    def has_content() -> bool:
        if not file_path.exists():
            return False
        try:
            return bool(file_path.read_text().strip())
        except OSError:
            return False

    return poll_until(has_content, timeout=timeout)


@dataclass
class WorkrexCommandResult:
    """Represents the result of running a workrex command inside Rex."""

    exit_code: int
    stdout: str
    stderr: str


@pytest.fixture(scope="session")
def workrex_exe_path() -> Path:
    """
    Returns the path to the local workrex build for testing.
    """
    local_path = Path(
        os.environ.get(
            "WORKREX_TEST_BINARY",
            str(Path(__file__).parent.parent / f"target/debug/{BIN}"),
        )
    )
    if not local_path.exists():
        pytest.fail("Could not find workrex executable. Run 'cargo build' first.")
    return local_path


def write_workrex_config(
    repo_path: Path,
    panes: list[dict[str, Any]] | None = None,
    post_create: list[str] | None = None,
    pre_merge: list[str] | None = None,
    pre_remove: list[str] | None = None,
    files: dict[str, list[str]] | None = None,
    env: MuxEnvironment | None = None,
    window_prefix: str | None = None,
    agent: str | None = None,
    merge_strategy: str | None = None,
    merge_keep: bool | None = None,
    confirm_unmerged_removal: bool | None = None,
    worktree_naming: str | None = None,
    worktree_prefix: str | None = None,
    main_branch: str | None = None,
    base_branch: str | None = None,
    prompt_file_only: bool | None = None,
    window_placement: str | None = None,
    default_session: str | None = None,
    layouts: dict[str, Any] | None = None,
    status_icons: dict[str, str] | None = None,
):
    """Creates a .workrex.yaml file from structured data and optionally commits it."""
    # Disable nerdfonts by default to ensure consistent "wr-" prefix in tests,
    # regardless of user's global config
    config: dict[str, Any] = {"nerdfont": False}
    if panes is not None:
        config["panes"] = panes
    if layouts is not None:
        config["layouts"] = layouts
    if post_create:
        config["post_create"] = post_create
    if pre_merge:
        config["pre_merge"] = pre_merge
    if pre_remove:
        config["pre_remove"] = pre_remove
    if files:
        config["files"] = files
    if window_prefix:
        config["window_prefix"] = window_prefix
    if agent:
        config["agent"] = agent
    if merge_strategy:
        config["merge_strategy"] = merge_strategy
    if merge_keep is not None:
        config["merge_keep"] = merge_keep
    if confirm_unmerged_removal is not None:
        config["confirm_unmerged_removal"] = confirm_unmerged_removal
    if worktree_naming:
        config["worktree_naming"] = worktree_naming
    if worktree_prefix:
        config["worktree_prefix"] = worktree_prefix
    if main_branch:
        config["main_branch"] = main_branch
    if base_branch:
        config["base_branch"] = base_branch
    if prompt_file_only is not None:
        config["prompt_file_only"] = prompt_file_only
    if window_placement is not None:
        config["window_placement"] = window_placement
    if default_session is not None:
        config["default_session"] = default_session
    if status_icons:
        config["status_icons"] = status_icons
    (repo_path / ".workrex.yaml").write_text(yaml.dump(config))

    # If env is provided, commit the config file to avoid uncommitted changes in merge tests
    if env:
        subprocess.run(
            ["git", "add", ".workrex.yaml"], cwd=repo_path, check=True, env=env.env
        )
        subprocess.run(
            ["git", "commit", "-m", "Add workrex config"],
            cwd=repo_path,
            check=True,
            env=env.env,
        )


def write_global_workrex_config(
    env: MuxEnvironment,
    panes: list[dict[str, Any]] | None = None,
    post_create: list[str] | None = None,
    files: dict[str, list[str]] | None = None,
    window_prefix: str | None = None,
    agent: str | None = None,
    base_branch: str | None = None,
    merge_keep: bool | None = None,
    agents: dict[str, Any] | None = None,
    auto_name: dict[str, Any] | None = None,
) -> Path:
    """Creates the global ~/.config/workrex/config.yaml file within the isolated HOME."""
    config: dict[str, Any] = {"nerdfont": False}
    if env.backend_name == "rex":
        config["mode"] = "window"
    if panes is not None:
        config["panes"] = panes
    if post_create is not None:
        config["post_create"] = post_create
    if files is not None:
        config["files"] = files
    if window_prefix is not None:
        config["window_prefix"] = window_prefix
    if agent is not None:
        config["agent"] = agent
    if base_branch is not None:
        config["base_branch"] = base_branch
    if merge_keep is not None:
        config["merge_keep"] = merge_keep
    if agents is not None:
        config["agents"] = agents
    if auto_name is not None:
        config["auto_name"] = auto_name

    config_dir = env.home_path / ".config" / "workrex"
    config_dir.mkdir(parents=True, exist_ok=True)
    config_path = config_dir / "config.yaml"
    config_path.write_text(yaml.dump(config))
    return config_path


def get_worktree_path(repo_path: Path, branch_name: str) -> Path:
    """Returns the expected path for a worktree directory.

    The directory name is the slugified version of the branch name,
    matching the Rust workrex behavior.
    """
    handle = slugify(branch_name)
    return repo_path.parent / f"{repo_path.name}__worktrees" / handle


def get_window_name(branch_name: str) -> str:
    """Returns the expected Rex window name for a worktree.

    The window name uses the slugified version of the branch name,
    matching the Rust workrex behavior.
    """
    handle = slugify(branch_name)
    return f"{DEFAULT_WINDOW_PREFIX}{handle}"


# Global counter to generate unique script names
_script_counter = 0


def get_scripts_dir(env: MuxEnvironment) -> Path:
    """Get the directory for test helper scripts.

    Uses a directory in /tmp (outside the git repo) to avoid being
    affected by git operations like `git stash -u` which would stash
    untracked files in the repo.

    The scripts dir is cached on the env object to ensure consistent paths
    across multiple calls within the same test. Uses tempfile.mkdtemp for
    guaranteed uniqueness in parallel test runs.
    """
    # Cache the scripts dir on the env object for consistent paths
    if env._scripts_dir is None:
        # Use mkdtemp for guaranteed uniqueness (avoids hash collisions)
        env._scripts_dir = Path(tempfile.mkdtemp(prefix="workrex_scripts_"))
    return env._scripts_dir


def make_env_script(env: MuxEnvironment, command: str, env_vars: dict[str, str]) -> str:
    """Create a script file that sets environment variables and runs a command.

    This avoids terminal input line length limits when env vars or paths are long.

    Args:
        env: The multiplexer environment (provides tmp_path)
        command: The shell command to run
        env_vars: Environment variables to set (e.g., {"XDG_STATE_HOME": "/path"})

    Returns:
        Shell command that can be passed to send_keys
    """
    global _script_counter
    _script_counter += 1
    script_file = get_scripts_dir(env) / f"env_cmd_{_script_counter}.sh"

    exports = "\n".join(f"export {k}={shlex.quote(v)}" for k, v in env_vars.items())
    script_content = f"""#!/bin/sh
{exports}
{command}
"""
    script_file.write_text(script_content)
    # Invoke the interpreter directly: cold shebang execution on macOS can
    # stall before the script body starts, consuming the command's wait budget.
    return f"/bin/sh {shlex.quote(str(script_file))}"


def get_session_name(branch_name: str) -> str:
    """Returns the expected Rex session name for a worktree created with --session.

    The session name uses the slugified version of the branch name,
    matching the Rust workrex behavior.
    """
    handle = slugify(branch_name)
    return f"{DEFAULT_WINDOW_PREFIX}{handle}"


def run_workrex_command(
    env: MuxEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    command: str,
    pre_run_mux_cmds: list[list[str]] | None = None,
    expect_fail: bool = False,
    working_dir: Path | None = None,
    stdin_input: str | None = None,
    pre_run_env: dict | None = None,
    timeout: float = 10.0,
) -> WorkrexCommandResult:
    """
    Helper to run a workrex command inside the isolated multiplexer session.

    Allows tests to optionally expect failure while still capturing stdout/stderr.

    Args:
        env: The isolated multiplexer environment
        workrex_exe_path: Path to the workrex executable
        repo_path: Path to the git repository
        command: The workrex command to run (e.g., "add feature-branch")
        pre_run_mux_cmds: Optional list of mux commands to run before the command
        expect_fail: Whether the command is expected to fail (non-zero exit)
        working_dir: Optional directory to run the command from (defaults to repo_path)
        stdin_input: Optional text to pipe to the command's stdin
        pre_run_env: Optional dict of environment variables to export before running
        timeout: Seconds to wait for the command to complete
    """
    scripts_dir = get_scripts_dir(env)
    stdout_file = scripts_dir / "workrex_stdout.txt"
    stderr_file = scripts_dir / "workrex_stderr.txt"
    exit_code_file = scripts_dir / "workrex_exit_code.txt"
    script_file = scripts_dir / "workrex_run.sh"

    for f in [stdout_file, stderr_file, exit_code_file]:
        if f.exists():
            f.unlink()

    if pre_run_mux_cmds:
        for cmd_args in pre_run_mux_cmds:
            env.mux_command(cmd_args)

    workdir = working_dir if working_dir is not None else repo_path

    # Handle stdin piping via printf
    pipe_cmd = ""
    if stdin_input is not None:
        pipe_cmd = f"printf %s {shlex.quote(stdin_input)} | "

    # Build extra env exports
    extra_env_lines = ""
    if pre_run_env:
        extra_env_lines = (
            "\n".join(f"export {k}={shlex.quote(v)}" for k, v in pre_run_env.items())
            + "\n"
        )

    # Write the command to a script file to avoid terminal input line length limits.
    # The PATH can be very long in test environments, causing command truncation.
    exit_trap = f"echo $? > {shlex.quote(str(exit_code_file))}"
    script_content = f"""#!/bin/sh
trap {shlex.quote(exit_trap)} EXIT
export PATH={shlex.quote(env.env["PATH"])}
export TMPDIR={shlex.quote(env.env.get("TMPDIR", "/tmp"))}
export HOME={shlex.quote(env.env.get("HOME", ""))}
export SHELL={shlex.quote(env.env.get("SHELL", os.environ.get("SHELL", "/bin/sh")))}
export WORKREX_TEST=1
{extra_env_lines}cd {shlex.quote(str(workdir))}
{pipe_cmd}{shlex.quote(str(workrex_exe_path))} {command} > {shlex.quote(str(stdout_file))} 2> {shlex.quote(str(stderr_file))}
"""
    script_file.write_text(script_content)

    # Read the script through its interpreter, avoiding cold shebang startup
    # delays before the command can execute (as in make_env_script).
    env.send_keys("test:", f"/bin/sh {shlex.quote(str(script_file))}", enter=True)

    if not poll_until_file_has_content(exit_code_file, timeout=timeout):
        # Capture pane content for debugging
        pane_content = env.capture_pane("test") or "(empty)"
        raise AssertionError(
            f"workrex command did not complete in time\nPane content:\n{pane_content}"
        )

    result = WorkrexCommandResult(
        exit_code=int(exit_code_file.read_text().strip()),
        stdout=stdout_file.read_text() if stdout_file.exists() else "",
        stderr=stderr_file.read_text() if stderr_file.exists() else "",
    )

    if expect_fail:
        if result.exit_code == 0:
            raise AssertionError(
                f"workrex {command} was expected to fail but succeeded.\nStdout:\n{result.stdout}"
            )
    else:
        if result.exit_code != 0:
            raise AssertionError(
                f"workrex {command} failed with exit code {result.exit_code}\n{result.stderr}"
            )

    return result


def run_workrex_add(
    env: MuxEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    branch_name: str,
    pre_run_mux_cmds: list[list[str]] | None = None,
    *,
    base: str | None = None,
    background: bool = False,
    config: Path | None = None,
) -> None:
    """
    Helper to run `workrex add` command inside the isolated multiplexer session.

    Asserts that the command completes successfully.

    Args:
        env: The isolated multiplexer environment
        workrex_exe_path: Path to the workrex executable
        repo_path: Path to the git repository
        branch_name: Name of the branch/worktree to create
        pre_run_mux_cmds: Optional list of mux commands to run before workrex add
        base: Optional base branch for the new worktree (passed as `--base`)
        background: If True, pass `--background` so the window is created without focus
        config: Optional path to an alternate config file (passed as `--config`)
    """
    args = ["add", branch_name]
    if base:
        args.extend(["--base", base])
    if background:
        args.append("--background")
    if config:
        args.extend(["--config", str(config)])

    command = " ".join(args)

    run_workrex_command(
        env,
        workrex_exe_path,
        repo_path,
        command,
        pre_run_mux_cmds=pre_run_mux_cmds,
    )


def run_workrex_open(
    env: MuxEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    branch_name: str | list[str] | None = None,
    *,
    run_hooks: bool = False,
    force_files: bool = False,
    new_window: bool = False,
    session: bool = False,
    mode: str | None = None,
    target_name: str | None = None,
    parent_session: str | None = None,
    prompt: str | None = None,
    prompt_file: Path | None = None,
    pre_run_mux_cmds: list[list[str]] | None = None,
    expect_fail: bool = False,
    working_dir: Path | None = None,
    config: Path | None = None,
) -> WorkrexCommandResult:
    """
    Helper to run `workrex open` command inside the isolated multiplexer session.

    Returns the command result so tests can assert on stdout/stderr.

    Args:
        branch_name: Worktree name(s) to open. Can be a single string, a list of
            strings, or None (optional with --new, uses current directory).
        new_window: If True, pass --new to force opening a new window (creates suffix like -2, -3)
        session: If True, pass -s to force opening as a Rex session
        prompt: Inline prompt text to pass via -p
        prompt_file: Path to a prompt file to pass via -P
        working_dir: Optional directory to run the command from (defaults to repo_path)
        config: Optional path to an alternate config file (passed as `--config`)
    """
    flags: list[str] = []
    if run_hooks:
        flags.append("--run-hooks")
    if force_files:
        flags.append("--force-files")
    if new_window:
        flags.append("--new")
    if session:
        flags.append("-s")
    if mode:
        flags.append(f"--mode {shlex.quote(mode)}")
    if target_name:
        flags.append(f"--target-name {shlex.quote(target_name)}")
    if parent_session:
        flags.append(f"--parent-session {shlex.quote(parent_session)}")
    if prompt:
        flags.append(f"-p {shlex.quote(prompt)}")
    if prompt_file:
        flags.append(f"-P {shlex.quote(str(prompt_file))}")
    if config:
        flags.append(f"--config {shlex.quote(str(config))}")

    flag_str = f" {' '.join(flags)}" if flags else ""
    if isinstance(branch_name, list):
        name_part = " " + " ".join(branch_name) if branch_name else ""
    else:
        name_part = f" {branch_name}" if branch_name else ""
    return run_workrex_command(
        env,
        workrex_exe_path,
        repo_path,
        f"open{name_part}{flag_str}",
        pre_run_mux_cmds=pre_run_mux_cmds,
        expect_fail=expect_fail,
        working_dir=working_dir,
    )


def create_commit(env: MuxEnvironment, path: Path, message: str):
    """Creates and commits a file within the test env at a specific path."""
    (path / f"file_for_{message.replace(' ', '_').replace(':', '')}.txt").write_text(
        f"content for {message}"
    )
    env.run_command(["git", "add", "."], cwd=path)
    env.run_command(["git", "commit", "-m", message], cwd=path)


def create_dirty_file(path: Path, filename: str = "dirty.txt"):
    """Creates an uncommitted file."""
    (path / filename).write_text("uncommitted changes")


def run_workrex_remove(
    env: MuxEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    branch_name: str | None = None,
    force: bool = False,
    keep_branch: bool = False,
    gone: bool = False,
    all: bool = False,
    user_input: str | None = None,
    expect_fail: bool = False,
    from_window: str | None = None,
) -> None:
    """
    Helper to run `workrex remove` command inside the isolated multiplexer session.

    Uses background execution to avoid hanging when remove kills its own window.
    Asserts that the command completes successfully unless expect_fail is True.

    Args:
        env: The isolated multiplexer environment
        workrex_exe_path: Path to the workrex executable
        repo_path: Path to the git repository
        branch_name: Optional name of the branch/worktree to remove (omit to auto-detect from current branch)
        force: Whether to use -f flag to skip confirmation
        keep_branch: Whether to use --keep-branch flag to keep the local branch
        gone: Whether to use --gone flag to remove worktrees with deleted upstreams
        all: Whether to use --all flag to remove all worktrees
        user_input: Optional string to pipe to stdin (e.g., 'y' for confirmation)
        expect_fail: If True, asserts the command fails (non-zero exit code)
        from_window: Optional window name to run the command from (useful for testing remove from within worktree window)
    """
    scripts_dir = get_scripts_dir(env)
    stdout_file = scripts_dir / "workrex_remove_stdout.txt"
    stderr_file = scripts_dir / "workrex_remove_stderr.txt"
    exit_code_file = scripts_dir / "workrex_remove_exit_code.txt"

    # Clean up any previous files
    for f in [stdout_file, stderr_file, exit_code_file]:
        if f.exists():
            f.unlink()

    force_flag = "-f " if force else ""
    keep_branch_flag = "--keep-branch " if keep_branch else ""
    gone_flag = "--gone " if gone else ""
    all_flag = "--all " if all else ""
    branch_arg = branch_name if branch_name else ""
    input_cmd = f"echo '{user_input}' | " if user_input else ""
    started_file: Path | None = None

    if from_window:
        worktree_path = get_worktree_path(
            repo_path, from_window.replace(DEFAULT_WINDOW_PREFIX, "")
        )
        started_file = scripts_dir / "workrex_remove_started.txt"
        if started_file.exists():
            started_file.unlink()
        remove_script = (
            f"echo started > {shlex.quote(str(started_file))}; "
            f"cd {shlex.quote(str(worktree_path))} && "
            f"{input_cmd}"
            f"{shlex.quote(str(workrex_exe_path))} remove {force_flag}{keep_branch_flag}{gone_flag}{all_flag}{branch_arg} "
            f"> {shlex.quote(str(stdout_file))} 2> {shlex.quote(str(stderr_file))}; "
            f"echo $? > {shlex.quote(str(exit_code_file))}"
        )
        env.send_keys(from_window, remove_script)
    else:
        remove_script = (
            f"cd {repo_path} && "
            f"{input_cmd}"
            f"{workrex_exe_path} remove {force_flag}{keep_branch_flag}{gone_flag}{all_flag}{branch_arg} "
            f"> {stdout_file} 2> {stderr_file}; "
            f"echo $? > {exit_code_file}"
        )

        env.run_shell_background(remove_script)

    if from_window:
        assert started_file is not None
        assert poll_until_file_has_content(started_file, timeout=5.0), (
            "workrex remove did not start in target window"
        )
        if not expect_fail:
            assert poll_until(
                lambda: from_window not in env.list_windows(), timeout=15.0
            ), "workrex remove did not close the target window"
            branch_for_path = branch_name or from_window.replace(
                DEFAULT_WINDOW_PREFIX, ""
            )
            worktree_path = get_worktree_path(repo_path, branch_for_path)
            assert poll_until(lambda: not worktree_path.exists(), timeout=15.0), (
                "workrex remove did not remove the worktree"
            )
            if not keep_branch:
                assert poll_until(
                    lambda: (
                        branch_for_path
                        not in env.run_command(
                            ["git", "branch", "--list", branch_for_path], cwd=repo_path
                        ).stdout
                    ),
                    timeout=15.0,
                ), "workrex remove did not delete the branch"
            return

    # Wait for command to complete (longer timeout for --gone which runs git fetch)
    assert poll_until_file_has_content(exit_code_file, timeout=15.0), (
        "workrex remove did not complete in time"
    )

    exit_code = int(exit_code_file.read_text().strip())
    stderr = stderr_file.read_text() if stderr_file.exists() else ""

    if expect_fail:
        if exit_code == 0:
            raise AssertionError(
                f"workrex remove was expected to fail but succeeded.\nStderr:\n{stderr}"
            )
    else:
        if exit_code != 0:
            raise AssertionError(
                f"workrex remove failed with exit code {exit_code}\nStderr:\n{stderr}"
            )


def run_workrex_merge(
    env: MuxEnvironment,
    workrex_exe_path: Path,
    repo_path: Path,
    branch_name: str | None = None,
    ignore_uncommitted: bool = False,
    rebase: bool = False,
    squash: bool = False,
    keep: bool = False,
    cleanup: bool = False,
    into: str | None = None,
    no_verify: bool = False,
    no_hooks: bool = False,
    notification: bool = False,
    expect_fail: bool = False,
    from_window: str | None = None,
) -> None:
    """
    Helper to run `workrex merge` command inside the isolated multiplexer session.

    Uses background execution to avoid hanging when merge kills its own window.
    Asserts that the command completes successfully unless expect_fail is True.

    Args:
        env: The isolated multiplexer environment
        workrex_exe_path: Path to the workrex executable
        repo_path: Path to the git repository
        branch_name: Optional name of the branch to merge (omit to auto-detect from current branch)
        ignore_uncommitted: Whether to use --ignore-uncommitted flag
        rebase: Whether to use --rebase flag
        squash: Whether to use --squash flag
        keep: Whether to use --keep flag
        cleanup: Whether to use --cleanup flag
        into: Optional target branch to merge into (instead of main)
        no_verify: Whether to use --no-verify flag (skip pre-merge hooks)
        notification: Whether to use --notification flag (show system notification)
        expect_fail: If True, asserts the command fails (non-zero exit code)
        from_window: Optional window name to run the command from
    """
    scripts_dir = get_scripts_dir(env)
    stdout_file = scripts_dir / "workrex_merge_stdout.txt"
    stderr_file = scripts_dir / "workrex_merge_stderr.txt"
    exit_code_file = scripts_dir / "workrex_merge_exit_code.txt"

    for f in [stdout_file, stderr_file, exit_code_file]:
        if f.exists():
            f.unlink()

    flags = []
    if ignore_uncommitted:
        flags.append("--ignore-uncommitted")
    if rebase:
        flags.append("--rebase")
    if squash:
        flags.append("--squash")
    if keep:
        flags.append("--keep")
    if cleanup:
        flags.append("--cleanup")
    if into:
        flags.append(f"--into {into}")
    if no_verify:
        flags.append("--no-verify")
    if no_hooks:
        flags.append("--no-hooks")
    if notification:
        flags.append("--notification")

    branch_arg = branch_name if branch_name else ""
    flags_str = " ".join(flags)

    started_file: Path | None = None
    if from_window:
        from_branch = from_window.replace(DEFAULT_WINDOW_PREFIX, "")
        worktree_path = get_worktree_path(repo_path, from_branch)
        workdir = worktree_path
        started_file = scripts_dir / "workrex_merge_started.txt"
        if started_file.exists():
            started_file.unlink()
    else:
        workdir = repo_path

    # Create a simple editor script for non-interactive git commits
    editor_script = scripts_dir / "git_editor.sh"
    editor_script.write_text('#!/bin/sh\necho "Auto commit from test" > "$1"\n')
    editor_command = f"/bin/sh {shlex.quote(str(editor_script))}"

    started_command = (
        f"echo started > {shlex.quote(str(started_file))}; " if started_file else ""
    )
    merge_script = (
        f"{started_command}"
        f"export GIT_EDITOR={shlex.quote(editor_command)} && "
        f"cd {shlex.quote(str(workdir))} && "
        f"{shlex.quote(str(workrex_exe_path))} merge {flags_str} {branch_arg} "
        f"> {shlex.quote(str(stdout_file))} 2> {shlex.quote(str(stderr_file))}; "
        f"echo $? > {shlex.quote(str(exit_code_file))}"
    )

    if from_window:
        env.send_keys(from_window, merge_script)
        assert started_file is not None
        assert poll_until_file_has_content(started_file, timeout=5.0), (
            "workrex merge did not start in target window"
        )
    else:
        env.run_shell_background(merge_script)

    assert poll_until_file_has_content(exit_code_file, timeout=10.0), (
        "workrex merge did not complete in time"
    )

    exit_code = int(exit_code_file.read_text().strip())
    stderr = stderr_file.read_text() if stderr_file.exists() else ""

    if expect_fail:
        if exit_code == 0:
            raise AssertionError(
                f"workrex merge was expected to fail but succeeded.\nStderr:\n{stderr}"
            )
    else:
        if exit_code != 0:
            raise AssertionError(
                f"workrex merge failed with exit code {exit_code}\nStderr:\n{stderr}"
            )


def install_fake_gh_cli(
    env: MuxEnvironment,
    pr_number: int,
    json_response: dict[str, Any] | None = None,
    stderr: str = "",
    exit_code: int = 0,
):
    """
    Creates a fake 'gh' command that responds to 'pr view <number> --json' with controlled output.

    Args:
        env: The isolated multiplexer environment
        pr_number: The PR number to respond to
        json_response: Dict containing the PR data to return as JSON (or None to return error)
        stderr: Error message to output to stderr
        exit_code: Exit code for the fake gh command (0 for success, non-zero for error)
    """

    # Create a bin directory in the test home
    bin_dir = env.home_path / "bin"
    bin_dir.mkdir(exist_ok=True)

    # Create the fake gh script
    gh_script = bin_dir / "gh"

    # Build the script content
    json_output = json.dumps(json_response) if json_response else ""

    # Escape single quotes in JSON for shell script
    json_output_escaped = json_output.replace("'", "'\\''")

    script_content = f"""#!/bin/sh
# Fake gh CLI for testing

# Check if this is a 'pr view' command for our PR number
# The command will be: gh pr view {pr_number} --json fields...
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$3" = "{pr_number}" ]; then
    if [ {exit_code} -ne 0 ]; then
        echo "{stderr}" >&2
        exit {exit_code}
    fi
    echo '{json_output_escaped}'
    exit 0
fi

# For any other command, fail
echo "gh: command not implemented in fake" >&2
exit 1
"""

    env.install_script(gh_script, script_content)

    # Add the bin directory to PATH
    new_path = f"{bin_dir}:{env.env.get('PATH', '')}"
    env.env["PATH"] = new_path
    # Set PATH in the multiplexer session so workrex can find the fake gh
    env.set_session_env("PATH", new_path)


def pytest_report_teststatus(report):
    """Suppress progress dots when running in Claude Code."""
    import os

    if os.environ.get("CLAUDECODE") and report.when == "call" and report.passed:
        return report.outcome, "", report.outcome.upper()
