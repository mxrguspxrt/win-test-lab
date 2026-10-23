#!/usr/bin/env bash
# tests/parity.sh - compare wtl-rs with the Python tools in ../lib that build-image.sh and
# launch-test.sh still use, so both sides write the same discs and read the same output.
#   wtl-rs/tests/parity.sh            (needs python3; builds wtl-rs first)
# Checks: mkiso images byte for byte (only the volume-descriptor timestamps may differ) and
# outputs.py decoding of real serial logs.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
cargo build --quiet --manifest-path "$HERE/Cargo.toml" || exit 2
RS="$HERE/target/debug/wtl-rs"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/wtl-parity.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
rc=0 n=0
pass() { n=$((n + 1)); }
fail() { echo "FAIL $*"; rc=1; }

# ---------------------------------------------------------------- mkiso
mask() { # ISO -> copy with the PVD/SVD creation+modification times and root-record dates zeroed
    python3 - "$1" "$1.masked" <<'PY'
import sys
b = bytearray(open(sys.argv[1], "rb").read())
for sec in (16, 17):
    o = sec * 2048
    b[o + 156 + 18:o + 156 + 25] = bytes(7)
    b[o + 813:o + 847] = bytes(34)
open(sys.argv[2], "wb").write(b)
PY
}
same_iso() { # label src volid
    rm -f "$TMP/p.iso" "$TMP/r.iso"
    python3 "$ROOT/lib/mkiso.py" -o "$TMP/p.iso" -V "$3" "$2" 2>"$TMP/pe"; local pc=$?
    "$RS" mkiso -o "$TMP/r.iso" -V "$3" "$2" 2>"$TMP/re"; local rc_=$?
    if [[ $pc != 0 || $rc_ != 0 ]]; then
        [[ $pc == "$rc_" ]] && pass || fail "mkiso $1: exit python $pc rust $rc_ ($(cat "$TMP/pe") / $(cat "$TMP/re"))"
        return
    fi
    mask "$TMP/p.iso"; mask "$TMP/r.iso"
    if cmp -s "$TMP/p.iso.masked" "$TMP/r.iso.masked"; then pass
    else fail "mkiso $1: images differ ($(cmp "$TMP/p.iso.masked" "$TMP/r.iso.masked" | head -1))"; fi
}
T="$TMP/tree"
mkdir -p "$T/LAB/sub dir/deeper" "$T/a.b.c" "$T/UPPER" "$T/upper2" "$T/.dotdir" "$T/ünï cødé"
printf 'x' >"$T/LAB/lab.json"
printf 'hello' >"$T/LAB/sub dir/Some Long File Name With Spaces And More Words.txt"
printf '' >"$T/empty"
printf 'h' >"$T/.hidden"
printf 'a' >"$T/a-b.txt"; printf 'b' >"$T/a_b.txt"; printf 'c' >"$T/A+B.TXT"
printf 'v' >"$T/ünï cødé/fïlé.exe"
printf 'z' >"$T/a.b.c/name.with.many.dots.extension_long"
for k in $(seq 1 40); do printf '%s' "$k" >"$T/LAB/sub dir/deeper/file-number-$k-with-a-long-common-prefix.dat"; done
for k in $(seq 1 12); do printf '%s' "$k" >"$T/Collide_Name_That_Is_Long_Enough_To_Be_Cut_$k.txt"; done
head -c 300000 /dev/urandom >"$T/big.bin"
same_iso "tricky tree" "$T" "WTL_LAB"
same_iso "guest/" "$ROOT/guest" "WTLX"
same_iso "lib/" "$ROOT/lib" "a long volume label, with commas"
[[ -d "$ROOT/examples/smoke-test/out" ]] && same_iso "smoke-test/out" "$ROOT/examples/smoke-test/out" "WTLX"
same_iso "missing dir" "$TMP/none" "X"

# ---------------------------------------------------------------- outputs
for s in "$ROOT"/labs/state/*/*.serial.log "$ROOT"/labs/state/*/runs/*/*.serial.log "$ROOT"/labs/state/*/runs/*/*/serial.log; do
    [[ -s "$s" ]] || continue
    rm -rf "$TMP/po" "$TMP/ro"
    pj="$(python3 "$ROOT/lib/outputs.py" "$s" "$TMP/po")"; rj="$("$RS" outputs "$s" "$TMP/ro")"
    if [[ "$pj" == "$rj" ]] && diff -r "$TMP/po" "$TMP/ro" >/dev/null; then pass
    else fail "outputs $s: [$pj] vs [$rj]"; fi
done

echo "parity: $n checks passed$( ((rc)) && echo ', SOME FAILED' )"
exit "$rc"
