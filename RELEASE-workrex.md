# Releasing workrex

The fork keeps the Cargo version equal to the upstream version it is based on.
The release workflow builds `workrex-darwin-arm64` and `workrex-linux-amd64` as
`.tar.gz` plus `.sha256` and publishes them on kong109/workrex. It does not
publish to crates.io or Homebrew, and it fails when the tag is not
`v<Cargo.toml version>` (`scripts/check-release-tag.sh`).

To release the upstream version the fork is on, tag the fork's `main` and push
the tag:

```bash
git tag v0.1.278 && git push origin v0.1.278
```

To re-release fork changes on the same upstream version:

1. Set `version` in `Cargo.toml` to `<version>-rex.N`, for example `0.1.278-rex.1`.
2. Run `cargo update --workspace` so `Cargo.lock` records the new version. The
   release builds with `--locked` and fails if `Cargo.lock` is stale.
3. Commit both files, then tag and push `v0.1.278-rex.1`.

`workrex update` orders `0.1.278-rex.1` after `0.1.278` and before `0.1.279`.
The next rebase onto a new upstream release drops the suffix. `workrex update`
and remote installs fetch the tag that matches the binary's Cargo version.
