# Notice

workrex is a fork of [workmux](https://github.com/raine/workmux) by Raine Virta
and the workmux contributors, used under the MIT License. The upstream copyright
and license text are kept unchanged in [LICENSE](LICENSE). Thanks to everyone who
built workmux.

## What the fork renames

Only the installed binary is renamed. It is `workrex`, so it can sit beside an
upstream `workmux` install. Self-references that a user or an agent runs go
through the binary name: the CLI name, `workrex update` (from
[kong109/workrex](https://github.com/kong109/workrex) releases), remote node
binaries, the status hook commands that `workrex setup` writes, and the bundled
skills.

These names stay the same as upstream, so the fork rebases cleanly and reads
existing workmux configuration:

- The Cargo package `workmux`
- Config, cache and state directories (`~/.config/workmux`, and so on)
- `.workmux.yaml` project config
- `WORKMUX_*` environment variables
- The Claude Code plugin id `workmux-status`

Log filters use the binary's crate name, so write `RUST_LOG=workrex=debug`.
