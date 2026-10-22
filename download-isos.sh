#!/usr/bin/env bash
# download-isos.sh - best-effort download of Windows Server evaluation ISOs (en-us, x64)
#   ./download-isos.sh [os...]      default: every OS without a matching ISO
# into $WTL_ISO_DIR. Resumable (.part files). Verifies the WIM build with isoinfo.py.
# The Microsoft fwlinks change from time to time; on failure the manual download page
# is printed and the script continues. Exit: 0 all present, 1 some missing.
set -euo pipefail
# shellcheck source=lib/common.sh
source "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/lib/common.sh"

declare -A URL=(
    [2012]='https://go.microsoft.com/fwlink/p/?LinkID=2195172&clcid=0x409&culture=en-us&country=US'
    [2016]='https://go.microsoft.com/fwlink/p/?LinkID=2195174&clcid=0x409&culture=en-us&country=US'
    [2019]='https://go.microsoft.com/fwlink/p/?LinkID=2195167&clcid=0x409&culture=en-us&country=US'
    [2022]='https://go.microsoft.com/fwlink/p/?LinkID=2195280&clcid=0x409&culture=en-us&country=US'
    [2025]='https://go.microsoft.com/fwlink/?linkid=2293312&clcid=0x409&culture=en-us&country=us'
)
declare -A FNAME=(
    [2012]='9600_SERVER_EVAL_x64FRE_en-us.iso'
    [2016]='14393_SERVER_EVAL_x64FRE_en-us.iso'
    [2019]='17763_SERVER_EVAL_x64FRE_en-us.iso'
    [2022]='20348_SERVER_EVAL_x64FRE_en-us.iso'
    [2025]='26100_SERVER_EVAL_x64FRE_en-us.iso'
)
manual_url() {
    local y="$1"; [[ "$y" == 2012 ]] && y=2012-r2
    echo "https://www.microsoft.com/en-us/evalcenter/download-windows-server-$y"
}

command -v curl >/dev/null 2>&1 || die "curl not found (sudo apt-get install curl)"
mkdir -p "$WTL_ISO_DIR"

KEYS=()
if (($#)); then
    for a in "$@"; do
        if [[ "$a" == all ]]; then mapfile -t KEYS < <(os_keys); continue; fi
        os_lookup "$a" || die "unknown OS '$a'"
        KEYS+=("$OS_KEY")
    done
else
    while IFS= read -r k; do
        find_iso "$k" >/dev/null || KEYS+=("$k")
    done < <(os_keys)
    ((${#KEYS[@]})) || { ok "all ISOs present in $WTL_ISO_DIR"; exit 0; }
fi

MISSING=()
for k in "${KEYS[@]}"; do
    os_lookup "$k"
    if iso="$(find_iso "$k")"; then
        ok "$k: already have $(basename "$iso")"
        continue
    fi
    url="${URL[$k]:-}"
    if [[ -z "$url" ]]; then
        warn "$k: no download link known; get it from $(manual_url "$k")"
        MISSING+=("$k"); continue
    fi
    out="$WTL_ISO_DIR/${FNAME[$k]}"
    part="$out.part"
    avail="$(free_gb "$WTL_ISO_DIR")"
    ((avail < 10)) && warn "only ${avail} GiB free in $WTL_ISO_DIR (an ISO is 5-8 GiB)"
    log "$k: downloading $OS_DISPLAY -> $out"
    log "    $url"
    rc=0
    curl -L --fail -C - --retry 3 --retry-delay 5 --connect-timeout 30 \
         -A 'Mozilla/5.0 (X11; Linux x86_64) win-test-lab' \
         -o "$part" "$url" || rc=$?
    if ((rc == 33)); then  # server refused resume - restart
        rm -f "$part"
        rc=0
        curl -L --fail --retry 3 --connect-timeout 30 -o "$part" "$url" || rc=$?
    fi
    if ((rc != 0)); then
        err "$k: download failed (curl exit $rc); partial data kept in $part"
        warn "$k: download manually from $(manual_url "$k") and save into $WTL_ISO_DIR"
        MISSING+=("$k"); continue
    fi
    b="$(py "$WTL_LIB/isoinfo.py" build "$part" 2>/dev/null || true)"
    if [[ "$b" != "$OS_BUILD" ]]; then
        err "$k: downloaded file is not the expected ISO (build '${b:-unreadable}', want $OS_BUILD)"
        mv -f "$part" "$out.bad"
        warn "$k: kept as $out.bad; download manually from $(manual_url "$k")"
        MISSING+=("$k"); continue
    fi
    mv -f "$part" "$out"
    ok "$k: $out (build $b)"
done

if ((${#MISSING[@]})); then
    warn "still missing: ${MISSING[*]}"
    exit 1
fi
exit 0
