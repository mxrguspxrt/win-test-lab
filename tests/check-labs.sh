#!/usr/bin/env bash
# quick sanity check: every labs/*.conf resolves its DC + client OS correctly.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
source lib/common.sh
rc=0
for c in labs/*.conf; do
    lab="$(basename "$c" .conf)"
    # strip inline comments, surrounding quotes and whitespace
    getval() { sed -n "s/^$1=//p" "$c" | head -1 | sed 's/#.*//; s/[[:space:]]*$//; s/^["'"'"']//; s/["'"'"']$//'; }
    dc="$(getval LAB_OS)"
    cl="$(getval LAB_CLIENT_OS)"
    [ -z "$cl" ] && cl="$dc"
    if ! os_lookup "$dc"; then echo "FAIL $lab: unknown LAB_OS $dc"; rc=1; continue; fi
    dck="$OS_KIND"
    if [ "$dck" != server ]; then echo "FAIL $lab: DC $dc is $dck, not server"; rc=1; fi
    dcfw="$OS_FIRMWARE"
    if ! os_lookup "$cl"; then echo "FAIL $lab: unknown LAB_CLIENT_OS $cl"; rc=1; continue; fi
    printf 'ok  %-7s DC=%s(server,%s)  client=%s(%s,%s)\n' "$lab" "$dc" "$dcfw" "$cl" "$OS_KIND" "$OS_FIRMWARE"
done
exit "$rc"
