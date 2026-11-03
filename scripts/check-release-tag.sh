#!/bin/sh
# Fail unless the release tag (default $GITHUB_REF_NAME) is v<Cargo.toml version>.
set -eu
tag=${1:-${GITHUB_REF_NAME:-}}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "${2:-Cargo.toml}" | head -n 1)
if [ "$tag" != "v$version" ]; then
	echo "Release tag '$tag' does not match Cargo.toml version $version (expected v$version)." >&2
	echo "Bump the version in Cargo.toml and Cargo.lock, commit, then tag. See RELEASE-workrex.md." >&2
	exit 1
fi
echo "Release tag $tag matches Cargo.toml"
