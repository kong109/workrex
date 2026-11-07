# Notice

Workrex derives from the MIT-licensed terminal-worktree project originally
authored by [Raine Virta](https://github.com/raine) and its contributors. The
original copyright notice and MIT license terms are preserved in
[LICENSE](LICENSE), alongside a separate copyright line for Workrex changes.
Thanks to everyone who built the original project.

## Workrex identity

This repository maintains Workrex as a Rex-only application. Its package,
installed binary, agent skills, status hooks, documentation, configuration,
cache, and state use the Workrex name.

- Binary and Cargo package: `workrex`.
- Project configuration: `.workrex.yaml`.
- Global configuration: `~/.config/workrex/config.yaml`.
- Environment variables: `WORKREX_*`.
- Remote node root: `~/.workrex-node`.
- Managed agent integrations: `workrex-status`.
- Logs: `$XDG_STATE_HOME/workrex/workrex.log`.

Rex owns terminal sessions, windows, and blocks. Workrex records the IDs it
creates and scopes terminal operations to the selected Rex server.

## Upstream history

Original history and attribution remain in Git. Active documentation describes
the behavior of this Rex application. Historical release notes for the original
application are available in earlier repository revisions and are not presented
as Workrex features.

The README merge helper remains available for maintainers carrying changes
across repository histories. Install it with `scripts/setup-readme-merge`; it
preserves the README identified by the `# workrex` heading. Its implementation
is in `scripts/merge-workrex-readme`.
