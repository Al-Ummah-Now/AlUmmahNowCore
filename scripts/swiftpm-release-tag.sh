#!/usr/bin/env bash
# Creates the SwiftPM release commit for a vX.Y.Z tag and pushes ONLY the plain X.Y.Z tag.
#
#   scripts/swiftpm-release-tag.sh <version> <xcframework-checksum> [remote] [AunCore.swift]
#
# Run from a checkout of the tagged commit (vX.Y.Z). The script:
#   1. creates a detached commit on top of it that points Package.swift at this release's
#      XCFramework (releaseVersion + releaseChecksum) and, if given, refreshes AunCore.swift;
#   2. tags that commit X.Y.Z (no "v") and pushes that tag only. No branch is ever pushed, so
#      main stays protected and needs no bypass. SwiftPM resolves by tag, so consumers using
#      `from: "X.Y.Z"` get the release commit.
# An existing X.Y.Z tag is only moved when it belongs to a re-run of the same release (its commit's
# parent is the same tagged commit); anything else is refused.
set -euo pipefail

VERSION="${1:?version (X.Y.Z)}"
CHECKSUM="${2:?XCFramework checksum}"
REMOTE="${3:-origin}"
SWIFT_SOURCES="${4:-}"

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || { echo "bad version: $VERSION" >&2; exit 1; }
[[ "$CHECKSUM" =~ ^[0-9a-f]{64}$ ]] || { echo "bad checksum: $CHECKSUM" >&2; exit 1; }

BASE=$(git rev-parse HEAD)
git checkout -q --detach "$BASE"

if [[ -n "$SWIFT_SOURCES" ]]; then
  cp "$SWIFT_SOURCES" apple/Sources/AunCore/AunCore.swift
fi
# Portable in-place edit (BSD and GNU sed).
sed -E -i.bak \
  -e "s/^let releaseVersion = \".*\"/let releaseVersion = \"$VERSION\"/" \
  -e "s/^let releaseChecksum = \".*\"/let releaseChecksum = \"$CHECKSUM\"/" \
  Package.swift
rm -f Package.swift.bak
grep -q "^let releaseVersion = \"$VERSION\"$" Package.swift || { echo "Package.swift: releaseVersion not updated" >&2; exit 1; }
grep -q "^let releaseChecksum = \"$CHECKSUM\"$" Package.swift || { echo "Package.swift: releaseChecksum not updated" >&2; exit 1; }

git add Package.swift apple/Sources/AunCore/AunCore.swift
# Same dates as the tagged commit, so re-running with the same artifact yields the same commit.
DATE=$(git show -s --format=%cI "$BASE")
GIT_AUTHOR_DATE="$DATE" GIT_COMMITTER_DATE="$DATE" \
  git -c user.name="github-actions[bot]" -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
  commit -q --allow-empty -m "release: Package.swift for v$VERSION [skip ci]"
RELEASE=$(git rev-parse HEAD)

FORCE=""
EXISTING=$(git ls-remote --tags "$REMOTE" "refs/tags/$VERSION" | cut -f1)
if [[ -n "$EXISTING" ]]; then
  git fetch -q "$REMOTE" "+refs/tags/$VERSION:refs/tags/$VERSION"
  OLD=$(git rev-parse "refs/tags/$VERSION^{commit}")
  if [[ "$OLD" == "$RELEASE" ]]; then
    echo "tag $VERSION already points at $RELEASE; nothing to do"
    exit 0
  fi
  OLD_PARENT=$(git rev-parse "$OLD^" 2>/dev/null || true)
  if [[ "$OLD_PARENT" != "$BASE" ]]; then
    echo "refusing to move tag $VERSION: it points at $OLD, which is not a release commit built on $BASE (v$VERSION)" >&2
    exit 1
  fi
  echo "re-run of the same release: moving tag $VERSION from $OLD to $RELEASE"
  FORCE="--force"
fi

git tag -f -a "$VERSION" -m "SwiftPM release for v$VERSION" "$RELEASE" >/dev/null
# Push the tag ref only. Never a branch.
git push $FORCE "$REMOTE" "refs/tags/$VERSION"
echo "SwiftPM tag $VERSION -> $RELEASE (parent $BASE)"
