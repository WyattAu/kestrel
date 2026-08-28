#!/usr/bin/env bash
# Builds the combined prepare-database for `cargo sqlx prepare`.
#
# ADR 0003 requires compile-time-checked queries, but kestrel-storage has
# TWO databases (ADR 0009). sqlx validates macros against a single
# DATABASE_URL schema, so we prepare against one SQLite file that carries
# BOTH schemas: every checked query in this crate is valid against it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATE="$ROOT/crates/kestrel-storage"
PREP_DIR="$CRATE/.prepare"
PREP_DB="$PREP_DIR/combined.db"

rm -rf "$PREP_DIR"
mkdir -p "$PREP_DIR"

for dir in cache data; do
  while IFS= read -r file; do
    echo "-- applying $dir/$(basename "$file")" >&2
    sqlite3 "$PREP_DB" < "$file"
  done < <(ls "$CRATE/migrations/$dir"/*.sql | sort)
done

echo "DATABASE_URL=sqlite://$PREP_DB"
