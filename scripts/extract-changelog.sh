#!/usr/bin/env bash
# Extract the body of a single CHANGELOG.md section, without its
# `## [X.Y.Z] - DATE` header.
#
# Usage: ./scripts/extract-changelog.sh <version>
#   e.g. ./scripts/extract-changelog.sh 1.3.0
#
# Output goes to stdout. Used by the GitHub Release workflow to feed the
# Added/Fixed/Changed/Removed sections into the release body so we don't
# have to babysit auto-generated commit lists.
#
# Assumes Keep-a-Changelog format: each version has a `## [X.Y.Z] - DATE`
# header, and the section ends at the next `## [` heading.

set -euo pipefail

VERSION="${1:?usage: $0 <version>}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHANGELOG="$SCRIPT_DIR/../CHANGELOG.md"

if [[ ! -f "$CHANGELOG" ]]; then
  echo "error: $CHANGELOG not found" >&2
  exit 1
fi

# Escape regex metacharacters in the version for sed.
VERSION_ESC="${VERSION//./\\.}"

# Range from the version header to the next version header. `sed '1d;$d'`
# strips the header line and the trailing next-version header so only the
# body (### Added / ### Fixed / ...) is emitted.
SECTION="$(sed -n "/^## \[${VERSION_ESC}\]/,/^## \[/p" "$CHANGELOG" | sed '1d;$d')"

if [[ -z "$SECTION" ]]; then
  echo "error: no CHANGELOG section found for version $VERSION" >&2
  exit 1
fi

printf '%s\n' "$SECTION"
