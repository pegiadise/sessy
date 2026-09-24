#!/usr/bin/env bash
# Print the GitHub release notes for a version: its CHANGELOG.md section plus
# the compare link. Used as:
#   gh release create v1.3.0 --title v1.3.0 --notes-file <(scripts/release-notes.sh v1.3.0)
set -euo pipefail
version="${1:?usage: $0 vX.Y.Z}"
version="${version#v}"
changelog="$(dirname "$0")/../CHANGELOG.md"

# Section body: everything after "## [X.Y.Z]" up to the next "## [" heading
# or the link definitions at the bottom of the file.
body="$(awk -v v="$version" '
  index($0, "## [" v "]") == 1 { on = 1; next }
  on && (/^## \[/ || /^\[[^]]+\]: /) { exit }
  on { print }
' "$changelog")"
if [[ -z "${body//[[:space:]]/}" ]]; then
  echo "error: no CHANGELOG.md section for $version" >&2
  exit 1
fi

link="$(awk -v v="$version" 'index($0, "[" v "]: ") == 1 { print substr($0, length(v) + 5) }' "$changelog")"

# Trim leading/trailing blank lines.
printf '%s\n' "$body" | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}'
# Only real comparisons; the first release has nothing to compare against.
if [[ "$link" == */compare/* ]]; then
  printf '\n**Full Changelog**: %s\n' "$link"
fi
