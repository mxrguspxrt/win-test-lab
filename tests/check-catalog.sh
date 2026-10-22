#!/usr/bin/env bash
# check that every images-list.conf key resolves to an ISO whose build matches the OS table.
# Warn (don't fail) when the ISO file is simply absent - a user may not have downloaded them all.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
source lib/common.sh
rc=0 present=0 absent=0
while IFS= read -r line; do
    line="${line%%#*}"
    [[ "$line" == *=* ]] || continue
    key="${line%%=*}"; file="${line#*=}"
    key="$(echo "$key" | xargs)"; file="$(echo "$file" | xargs)"
    [ -z "$key" ] && continue
    if ! os_lookup "$key"; then echo "FAIL $key: no oslist.conf entry"; rc=1; continue; fi
    want="$OS_BUILD"
    path="$file"; [[ "$path" == /* ]] || path="$WTL_ISO_DIR/$file"
    if [ ! -f "$path" ]; then printf 'warn %-14s %s (not downloaded)\n' "$key" "$file"; absent=$((absent+1)); continue; fi
    got="$(iso_build "$path" 2>/dev/null || true)"
    if [ "$got" = "$want" ]; then printf 'ok   %-14s %s (build %s)\n' "$key" "$file" "$got"; present=$((present+1))
    else echo "FAIL $key: $file is build ${got:-none}, expected $want"; rc=1; fi
done < images-list.conf
echo "catalog: $present present, $absent not downloaded"
exit "$rc"
