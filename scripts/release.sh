#!/usr/bin/env bash
# Release dde3-toolkit: bump the version, build the linux release binary
# into dist/ with a .sha256 checksum, commit, tag vX.Y.Z, push.
#
# Usage: scripts/release.sh <version>     e.g. scripts/release.sh 0.2.0
set -euo pipefail
cd "$(dirname "$0")/.."

V="${1:?usage: scripts/release.sh <version>}"
# Cargo.toml needs full semver; the tag keeps the given name (v0.1, v0.2.0, ...).
if [[ ! $V =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then V="$V.0"; TAG="v${1}"; else TAG="v$V"; fi
NAME=dde3-toolkit
DIST=dist
LINUX="$DIST/$NAME-linux-x86_64"

mkdir -p "$DIST"

# 1. Bump the crate version (first `version = "..."` in Cargo.toml).
sed -i "0,/^version = \".*\"/s//version = \"$V\"/" Cargo.toml

# 2. Linux (native).
cargo build --release
cp "target/release/$NAME" "$LINUX"
chmod +x "$LINUX"

# 3. Checksum (path relative to the repo root, like dde4).
sha256sum "$LINUX" > "$LINUX.sha256"

# 4. Commit, tag, push.
git add Cargo.toml "$DIST"
git commit -m "release: $TAG"
git tag -a "$TAG" -m "release $TAG"
git push origin main
git push origin "$TAG"

# 5. Gitea release with the dist/ files as assets (needs `gt`).
if command -v gt >/dev/null 2>&1; then
  gt release create "$TAG" \
    -n "Release $TAG — prebuilt binaries, verify with the .sha256 files." \
    --asset "$LINUX" --asset "$LINUX.sha256" \
    --verify-tag
else
  echo "WARNING: gt not found — skipped the Gitea release (assets are in dist/)." >&2
fi

echo
echo "$TAG released:"
ls -l "$DIST"
