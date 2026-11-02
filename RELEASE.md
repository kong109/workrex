# Releasing

## workrex fork releases

The fork keeps the Cargo version equal to the upstream version it is based on.
To release, tag the fork's `main` with `v<version>` and push the tag:

```bash
git tag v0.1.278 && git push origin v0.1.278
```

The release workflow builds `workrex-darwin-arm64` and `workrex-linux-amd64`
as `.tar.gz` plus `.sha256` and publishes them on kong109/workrex. It does not
publish to crates.io or Homebrew.

To re-release fork changes on the same upstream version, set the Cargo version
to `<version>-rex.N` (for example `0.1.278-rex.1`), commit, and tag
`v<version>-rex.N`. The next rebase onto a new upstream release drops the
suffix. `workrex update` and remote installs fetch the tag matching the
binary's Cargo version.

## Upstream releases

Requires [rust-release-tools](https://github.com/raine/rust-release-tools):

```bash
pipx install git+https://github.com/raine/rust-release-tools.git
```

To release:

```bash
just release
```

This will:

1. Bump the version in Cargo.toml
2. Generate a changelog entry using Claude
3. Open an editor to review the changelog
4. Commit, tag, and push

GitHub Actions then builds the release binaries, creates the GitHub release,
publishes the crate to crates.io, and updates the Homebrew tap. Transient
crates.io and network failures are retried automatically.

Crates.io publishing requires the `CARGO_REGISTRY_TOKEN` GitHub Actions secret.

## Backfilling changelog

To generate changelog entries for all git tags missing from CHANGELOG.md:

```bash
update-changelog
```

This uses `cc-batch` to process multiple tags in parallel.
