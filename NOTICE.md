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

Files both binaries would otherwise write get workrex's own name, so neither
install touches the other's: the default remote node root `~/.workrex-node` and
its `workrex-*` pins, the OpenCode plugin `workrex-status`, and the agent hook
files named `workrex-status`.

These names stay the same as upstream, so the fork rebases cleanly and reads
existing workmux configuration:

- The Cargo package `workmux`
- Config, cache and state directories (`~/.config/workmux`, and so on)
- `.workmux.yaml` project config
- `WORKMUX_*` environment variables
- The Claude Code plugin id `workmux-status`

Log filters use the binary's crate name, so write `RUST_LOG=workrex=debug`.

## Rebasing on upstream

The fork's [README.md](README.md) replaces upstream's. Upstream edits its
README often, so without help every rebase onto upstream stops on a README
conflict. A git merge driver resolves it. To install the driver, run this once
in each clone:

```bash
scripts/setup-readme-merge
```

The script copies [`scripts/merge-workrex-readme`](scripts/merge-workrex-readme)
into the git directory and adds `README.md merge=workrex-readme` to
`.git/info/attributes`. The driver keeps the README whose first line is
`# workrex` when only one side has it, and makes a normal three-way merge when
both or neither side has it. It works for `git rebase` and `git merge`.

The fork does not use `README.md merge=ours` in `.gitattributes`. A rebase
swaps the sides, so `ours` is upstream's copy and git drops the fork's README
with no conflict. A committed `.gitattributes` also does not apply to the
rebase steps before the commit that adds it.

Right after a rebase onto `upstream/main`, `ORIG_HEAD` is the old fork tip.
To read the upstream README changes that the rebase brought in, run:

```bash
git log -p ORIG_HEAD..upstream/main -- README.md
```

Carry over anything that workrex should also say.
