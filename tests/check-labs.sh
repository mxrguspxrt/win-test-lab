#!/usr/bin/env bash
# quick sanity check: every labs/*.json lab and examples/configs/*.json run loads and resolves
# (lab names, OS keys, users, steps, mounts), using the Rust `lab` itself.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
LAB="$ROOT/wtl-rs/target/debug/wtl-rs"
if [ ! -x "$LAB" ]; then
    (cd wtl-rs && cargo build --quiet) || { echo "FAIL cannot build wtl-rs"; exit 1; }
fi
rc=0
for f in labs/*.json examples/configs/*.json; do
    [ -f "$f" ] || continue
    if out="$("$LAB" lab status "$f" 2>&1)"; then
        printf 'ok  %-40s %s\n' "$f" "$(printf '%s\n' "$out" | head -1)"
    else
        echo "FAIL $f"; printf '%s\n' "$out" | sed 's/^/     /'; rc=1
    fi
done
exit "$rc"
