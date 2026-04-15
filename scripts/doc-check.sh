#!/usr/bin/env bash
# Verify that `cargo doc --no-deps` builds cleanly with rustdoc warnings denied.
#
# Catches broken intra-doc links, bare URLs, invalid codeblock attributes,
# and other rustdoc warnings that would otherwise rot silently.
#
# Usage: ./scripts/doc-check.sh

set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

export RUSTDOCFLAGS="-D warnings"
cargo doc --no-deps --workspace

echo "doc-check OK"
