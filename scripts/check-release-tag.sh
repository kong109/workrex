#!/bin/sh
# Fail unless the release tag (default $GITHUB_REF_NAME) is v<upstream version>-rex.<n>
# and equals v<Cargo.toml version>. A plain v<upstream version> is upstream's tag.
set -eu
tag=${1:-${GITHUB_REF_NAME:-}}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "${2:-Cargo.toml}" | head -n 1)
if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+-rex\.[1-9][0-9]*$'; then
	echo "Release tag '$tag' is not v<upstream version>-rex.<n>, for example v0.1.278-rex.1." >&2
	echo "A plain v<upstream version> collides with upstream's tag. See RELEASE-workrex.md." >&2
	exit 1
fi
if [ "$tag" != "v$version" ]; then
	echo "Release tag '$tag' does not match Cargo.toml version $version (expected v$version)." >&2
	echo "Set the version in Cargo.toml and Cargo.lock to ${tag#v}, commit, then tag. See RELEASE-workrex.md." >&2
	exit 1
fi
echo "Release tag $tag matches Cargo.toml"
