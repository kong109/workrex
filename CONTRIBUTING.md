# Contributing to Workrex

Workrex is a Rex application. Development, examples, and verification use Rex
sessions, windows, and blocks. Use the [Rex CLI guide](https://www.superlogical.com/rex/docs/)
when a workflow needs native terminal operations.

## Prerequisites

- Stable Rust with rustfmt and Clippy.
- Python 3.10 or newer, with a virtual environment at `tests/venv/`.
- Rex and Git.
- `just`, `checkle`, and Bun for the repository checks.

## Development setup

```bash
git clone https://github.com/kong109/workrex.git
cd workrex
cargo build --bin workrex
python3 -m venv tests/venv
tests/venv/bin/pip install -r tests/requirements.txt
cd docs
bun install --frozen-lockfile
```

Run `target/debug/workrex --help` from the repository root to use the development
binary. Point a new task at it explicitly rather than replacing a release
installation during unrelated development.

## Verification

```bash
# Rust formatting, static checks, unit tests, and documentation checks.
just check

# Rust unit tests.
cargo test --bin workrex

# Integration tests use isolated Rex servers.
just itest

# A focused integration test.
just itest tests/test_agent_state.py -vv
```

Rex test fixtures start a server on a private temporary socket, set `REX_SERVER`
for every child process, and terminate only the process they started. Never stop
the user's server to clean up a test. Keep all test session, window, and block
IDs scoped to that fixture's server.

Test behavior through the actual Rex APIs. Session labels, tab labels, block
IDs, and repository paths are distinct; a successful command against the wrong
target is a failed test. Cover both session mode and explicitly requested window
mode when placement or cleanup changes.

## Code and documentation

- Keep a change focused and preserve ownership checks before destructive actions.
- Run the checks relevant to the changed code, then exercise the user workflow.
- Update native skill source files and documentation together when commands change.
- Use `workrex`, `.workrex.yaml`, `~/.config/workrex`, and `WORKREX_*` consistently.
- Preserve the original author credit and license in [NOTICE.md](NOTICE.md) and
  [LICENSE](LICENSE).

Before a release, follow [RELEASE-workrex.md](RELEASE-workrex.md).
