#!/usr/bin/env bash
# Build the Exbar MSI installer.
#
# Prerequisites (one-time):
#   dotnet tool install --global wix
#   wix eula accept wix7
#   wix extension add --global WixToolset.Util.wixext
#
# Usage: ./scripts/build-msi.sh

set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

VERSION=$(grep '^version = ' "$REPO_ROOT/crates/exbar-cli/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
OUT="$REPO_ROOT/target/wix/exbar-${VERSION}-x64.msi"

echo "Building exbar ${VERSION} release binaries..."
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"

echo "Building MSI at ${OUT}..."
mkdir -p "$REPO_ROOT/target/wix"
wix build -arch x64 \
  -d "Version=${VERSION}" \
  -d "CargoTargetBinDir=$REPO_ROOT/target/release" \
  "$REPO_ROOT/crates/exbar-cli/wix/main.wxs" \
  -o "${OUT}" \
  -ext WixToolset.Util.wixext

echo "Done. MSI: ${OUT}"
