#!/usr/bin/env bash
# Verifies that every *relative* link in the repo's Markdown resolves to a
# file that exists (backlog #11). External URLs, mailto:, and pure #anchors
# are out of scope — cargo doc already guards intra-crate `[`code`]` refs.
#
# This catches the doc-drift class where prose points at a moved/renamed
# file (requirements.md -> docs/sync-engine.md, etc.) without needing
# network access in CI.
#
# Usage: scripts/check-doc-links.sh [paths...]   (default: tracked *.md)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Files we scan: explicit args, else every tracked .md (excluding target/).
if [ "$#" -gt 0 ]; then
  FILES=("$@")
else
  mapfile -t FILES < <(git ls-files '*.md' 2>/dev/null | grep -v '^target/')
fi

FAIL=0
COUNT=0

for md in "${FILES[@]}"; do
  [ -f "$md" ] || continue
  COUNT=$((COUNT + 1))
  mddir="$(dirname "$md")"

  # markdown [text](target) — capture the target up to the closing paren.
  while IFS= read -r target; do
    [ -n "$target" ] || continue

    # Skip non-file targets.
    case "$target" in
      http://*|https://*|mailto:*|ftp://*|\#*|\<*\>*) continue ;;
    esac

    # Strip an optional markdown title: `path "title"` / `path 'title'`.
    target="${target%% *}"
    # Strip a trailing heading anchor: docs/foo.md#section-1.
    path="${target%%#*}"
    [ -n "$path" ] || continue

    # Resolve relative to the markdown file's directory.
    resolved="$mddir/$path"
    if [ -e "$resolved" ]; then
      continue
    fi
    # Case-insensitive fallback (macOS/Windows checkouts).
    found="$(find "$mddir" -maxdepth 1 -iname "$(basename "$path")" -print -quit 2>/dev/null)"
    if [ -n "$found" ]; then
      continue
    fi
    echo "::error file=$md::$md: broken relative link -> $target"
    FAIL=1
  done < <(grep -oP '\]\(\K[^)]+' "$md" 2>/dev/null || true)
done

if [ "$FAIL" -eq 1 ]; then
  echo "Doc-link check failed: one or more relative links point at missing files." >&2
  exit 1
fi
echo "Doc-link check passed: all relative links in ${COUNT} file(s) resolve."
