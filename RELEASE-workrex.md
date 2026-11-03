# Releasing workrex

Fork releases are tagged `v<upstream version>-rex.<n>`, for example
`v0.1.278-rex.1`. A plain `v0.1.278` is upstream's tag, which the fork already
carries. The release workflow builds `workrex-darwin-arm64` and
`workrex-linux-amd64` as `.tar.gz` plus `.sha256` and publishes them on
kong109/workrex. It does not publish to crates.io or Homebrew. It fails unless
the tag has the `-rex.<n>` suffix and equals `v<Cargo.toml version>`
(`scripts/check-release-tag.sh`).

To release:

1. Set `version` in `Cargo.toml` to `<upstream version>-rex.<n>`, for example
   `0.1.278-rex.1`. Start `<n>` at 1 for each upstream version and add 1 for
   each later release on the same upstream version.
2. Run `cargo update --workspace` so `Cargo.lock` records the new version. The
   release builds with `--locked` and fails if `Cargo.lock` is stale.
3. Commit both files, then tag and push the tag:

   ```bash
   git tag v0.1.278-rex.1 && git push origin v0.1.278-rex.1
   ```

`workrex update` orders `0.1.278-rex.1` after `0.1.278` and before `0.1.279`.
A rebase onto a new upstream release resets the version to that release, so
set the suffix again before the next fork release. `workrex update` and remote
installs fetch the tag that matches the binary's Cargo version.
