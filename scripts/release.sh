#!/usr/bin/env bash
# Release dde3-toolkit: bump the version, build the Linux + Windows 10/11
# release binaries into dist/ with .sha256 checksums, commit, tag vX.Y.Z,
# push, and create the Gitea release.
#
# Usage: scripts/release.sh <version>     e.g. scripts/release.sh 0.2.0
# The Windows asset needs cargo-xwin (`cargo install cargo-xwin`) + wine +
# the x86_64-pc-windows-msvc rustup target. Plain MSVC cross-build — the
# Win7 delay-load RUSTFLAGS are NOT needed (target is Win10/11).
set -euo pipefail
cd "$(dirname "$0")/.."

V="${1:?usage: scripts/release.sh <version>}"
# Cargo.toml needs full semver; the tag keeps the given name (v0.1, v0.2.0, ...).
if [[ ! $V =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then V="$V.0"; TAG="v${1}"; else TAG="v$V"; fi
NAME=dde3-toolkit
DIST=dist
LINUX="$DIST/$NAME-linux-x86_64"
WINDOWS="$DIST/$NAME-windows-x86_64.exe"

mkdir -p "$DIST"

# 1. Bump the crate version (first `version = "..."` in Cargo.toml).
sed -i "0,/^version = \".*\"/s//version = \"$V\"/" Cargo.toml

# 2. Linux (native).
cargo build --release
cp "target/release/$NAME" "$LINUX"
chmod +x "$LINUX"

# 3. Windows 10/11 (xwin/MSVC cross-build; wine hosts the MSVC linker).
if ! cargo xwin --version >/dev/null 2>&1; then
  echo "ERROR: cargo-xwin not found — the Windows release asset is required."
  echo "       Install it with: cargo install cargo-xwin   (plus wine)." >&2
  exit 1
fi
rustup target add x86_64-pc-windows-msvc
cargo xwin build --release --target x86_64-pc-windows-msvc
cp "target/x86_64-pc-windows-msvc/release/$NAME.exe" "$WINDOWS"

# 4. Checksums (paths relative to the repo root, like dde4).
sha256sum "$LINUX" > "$LINUX.sha256"
sha256sum "$WINDOWS" > "$WINDOWS.sha256"

# 5. Commit, tag, push.
git add Cargo.toml "$DIST"
git commit -m "release: $TAG"
git tag -a "$TAG" -m "release $TAG"
git push origin main
git push origin "$TAG"

# 6. Gitea release with the dist/ files as assets (needs `gt`).
if command -v gt >/dev/null 2>&1; then
  gt release create "$TAG" \
    -n "Release $TAG — prebuilt binaries (Linux + Windows 10/11), verify with the .sha256 files." \
    --asset "$LINUX" --asset "$LINUX.sha256" \
    --asset "$WINDOWS" --asset "$WINDOWS.sha256" \
    --verify-tag
else
  echo "WARNING: gt not found — skipped the Gitea release (assets are in dist/)." >&2
fi

echo
echo "$TAG released:"
ls -l "$DIST"
