#!/usr/bin/env bash
# rename-isos.sh - give the Windows ISOs in WTL_ISO_DIR structured names:
#   win<os>_<lang>_<arch>_<build>.iso    e.g. win2019_en_x64_17763.iso
# The build number is read from inside each ISO, so this is driven by content, not the old
# name. Installer discs with no WIM (FOD/LOF, Vista client packs) are left untouched.
# Duplicate builds: the first becomes the canonical name, extras get a .dupN suffix.
# Dry run by default; pass --apply to actually rename.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISODIR="${WTL_ISO_DIR:-$(cd "$HERE/.." && pwd)/iso-files}"
APPLY=0; [[ "${1:-}" == --apply ]] && APPLY=1

# build number -> os token (client builds included)
os_token() {
    case "$1" in
        9600) echo win2012r2 ;; 14393) echo win2016 ;; 17763) echo win2019 ;;
        20348) echo win2022 ;; 26100) echo win2025 ;;
        19041) echo win10_22h2 ;; 19045) echo win10_22h2 ;;
        22000|22621|22631) echo win11_23h2 ;; 26100c) echo win11_24h2 ;; 26300) echo win11_24h2 ;;
        *) echo "" ;;
    esac
}

declare -A used
shopt -s nullglob nocaseglob
for f in "$ISODIR"/*.iso; do
    base="$(basename "$f")"
    build="$(python3 "$HERE/isoinfo.py" build "$f" 2>/dev/null || true)"
    [[ -z "$build" ]] && { printf 'skip  %-60s (no WIM - not an install disc)\n' "$base"; continue; }
    # client builds: 26100 appears for both Server 2025 and (rarely) Win11; disambiguate by name
    tok="$(os_token "$build")"
    if [[ "$build" == 26100 && "$base" == *[Cc]lient* ]]; then tok=win11_24h2; fi
    [[ -z "$tok" ]] && { printf 'skip  %-60s (build %s not mapped)\n' "$base" "$build"; continue; }
    want="${tok}_en_x64_${build}.iso"
    if [[ -n "${used[$want]:-}" ]]; then
        n=2; while [[ -n "${used[${want%.iso}.dup$n.iso]:-}" ]]; do n=$((n+1)); done
        want="${want%.iso}.dup$n.iso"
    fi
    used[$want]=1
    if [[ "$base" == "$want" ]]; then printf 'keep  %s\n' "$base"; continue; fi
    printf 'rename %-60s -> %s\n' "$base" "$want"
    ((APPLY)) && mv -n "$f" "$ISODIR/$want"
done
((APPLY)) || echo $'\n(dry run - pass --apply to rename)'
