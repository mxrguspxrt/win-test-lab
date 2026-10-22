#!/usr/bin/env bash
# build-image.sh - unattended Windows Server install + Windows Update -> images/<os>.qcow2
#
#   ./build-image.sh <os>... [--iso PATH] [--no-updates] [--disk-gb N] [--show]
#                            [--timeout-h H] [--force] [--name NAME] [--computer-name CN]
#
#   <os>   2012 (alias 2012r2) 2016 2019 2022 2025, or "all" (= every OS whose ISO is found)
#   --name NAME          image file images/NAME.qcow2 (default: the OS key; single OS only)
#   --computer-name CN   Windows computer name (default WTL-<os>; max 15 chars)
# Exit: 0 all built (or already present), 2 setup/build error, 3 timeout (worst code wins).
set -euo pipefail
# shellcheck source=lib/common.sh
source "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/lib/common.sh"

usage() {
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-64}"
}

OSES=() ISO_OVERRIDE="" UPDATES=true DISK_GB="" SHOW=0 TIMEOUT_H=12 FORCE=0 NAME_OVERRIDE="" CN_OVERRIDE=""
while (($#)); do
    case "$1" in
        --iso) ISO_OVERRIDE="${2:?--iso needs a path}"; shift 2 ;;
        --name) NAME_OVERRIDE="${2:?--name needs a name}"; shift 2 ;;
        --computer-name) CN_OVERRIDE="${2:?--computer-name needs a name}"; shift 2 ;;
        --no-updates) UPDATES=false; shift ;;
        --disk-gb) DISK_GB="${2:?}"; shift 2 ;;
        --show) SHOW=1; shift ;;
        --timeout-h) TIMEOUT_H="${2:?}"; shift 2 ;;
        --force) FORCE=1; shift ;;
        -h|--help) usage 0 ;;
        -*) err "unknown option $1"; usage 64 ;;
        *) OSES+=("$1"); shift ;;
    esac
done
((${#OSES[@]})) || usage 64
[[ -z "$DISK_GB" || "$DISK_GB" =~ ^[0-9]+$ ]] || die "--disk-gb must be an integer"
[[ "$TIMEOUT_H" =~ ^[0-9]+([.][0-9]+)?$ ]] || die "--timeout-h must be a number"
TIMEOUT_S="$(py -c 'import sys; print(int(float(sys.argv[1])*3600))' "$TIMEOUT_H")"

# ------------------------------------------------------------------ expand OS list
KEYS=()
for o in "${OSES[@]}"; do
    if [[ "$o" == all ]]; then
        while IFS= read -r k; do
            if find_iso "$k" >/dev/null; then KEYS+=("$k"); fi
        done < <(os_keys)
        ((${#KEYS[@]})) || die "all: no installation ISO found in $WTL_ISO_DIR"
    else
        os_lookup "$o" || die "unknown OS '$o' (known: $(os_keys | tr '\n' ' '))"
        KEYS+=("$OS_KEY")
    fi
done
# de-duplicate, keep order
mapfile -t KEYS < <(printf '%s\n' "${KEYS[@]}" | awk '!seen[$0]++')
if [[ -n "$ISO_OVERRIDE$NAME_OVERRIDE$CN_OVERRIDE" && ${#KEYS[@]} -gt 1 ]]; then
    die "--iso / --name / --computer-name can only be used with a single OS"
fi
[[ -z "$NAME_OVERRIDE" || "$NAME_OVERRIDE" =~ ^[A-Za-z0-9._-]+$ ]] || die "--name: letters, digits, . _ - only"
[[ -z "$CN_OVERRIDE" || "$CN_OVERRIDE" =~ ^[A-Za-z0-9-]{1,15}$ ]] || die "--computer-name: 1-15 letters, digits, -"

check_tools "$WTL_QEMU" "$WTL_QEMU_IMG"
mkdir -p "$WTL_IMAGES_DIR" "$WTL_RUNS_DIR"

# ------------------------------------------------------------------ cleanup / trap
CUR_PID="" CUR_SOCK=""
cleanup() {
    if [[ -n "$CUR_PID" ]]; then
        warn "stopping VM (pid $CUR_PID)"
        stop_vm "$CUR_PID" "$CUR_SOCK" 10
        CUR_PID=""
    fi
}
on_signal() {
    trap - INT TERM
    echo >&2
    warn "interrupted - partial image kept as images/<os>.qcow2.partial"
    kill_bg
    cleanup
    exit 2
}
trap on_signal INT TERM
trap cleanup EXIT

xml_escape() { py -c 'import sys,xml.sax.saxutils as s; print(s.escape(sys.argv[1], {"\"":"&quot;","'"'"'":"&apos;"}), end="")' "$1"; }

# render TEMPLATE OUT KEY=VAL...
render_template() {
    py - "$@" <<'EOF'
import sys, re
src, dst = sys.argv[1], sys.argv[2]
subs = dict(a.split("=", 1) for a in sys.argv[3:])
t = open(src, encoding="utf-8-sig").read().replace("\r\n", "\n")
for k, v in subs.items():
    t = t.replace("@%s@" % k, v)
left = sorted(set(re.findall(r"@[A-Z][A-Z0-9_]*@", t)))
if left:
    sys.stderr.write("warning: unreplaced placeholders in %s: %s\n" % (src, " ".join(left)))
open(dst, "w", encoding="utf-8", newline="\r\n").write(t)
EOF
}

# ------------------------------------------------------------------ one build
build_one() {
    local key="$1" iso idx disk_gb run_dir stage partial final sock serial t0 rc img cn
    os_lookup "$key"
    img="${NAME_OVERRIDE:-$OS_KEY}" cn="${CN_OVERRIDE:-WTL-$OS_KEY}"
    log "=== building $OS_DISPLAY ($OS_KEY) -> $img.qcow2, computer $cn ==="
    final="$WTL_IMAGES_DIR/$img.qcow2"
    partial="$final.partial"
    if [[ -f "$final" && $FORCE -eq 0 ]]; then
        ok "$final already exists (use --force to rebuild)"
        return 0
    fi

    if [[ -n "$ISO_OVERRIDE" ]]; then
        iso="$ISO_OVERRIDE"
        [[ -f "$iso" ]] || { err "ISO not found: $iso"; return 2; }
    else
        iso="$(find_iso "$OS_KEY")" || {
            err "no ISO for $OS_DISPLAY in $WTL_ISO_DIR (build $OS_BUILD; see images-list.conf)."
            err "Run ./download-isos.sh $OS_KEY or pass --iso PATH."
            return 2
        }
    fi
    iso="$(readlink -f "$iso")"
    log "ISO: $iso"

    if idx="$(py "$WTL_LIB/isoinfo.py" pick "$iso" "$OS_EDITION" "$OS_INST_TYPE" 2>/dev/null)"; then
        log "image index $idx ($OS_EDITION / $OS_INST_TYPE)"
    else
        rc=$?
        if ((rc == 2)); then err "$iso has no sources/install.wim - not an installation disc"; return 2; fi
        idx="$OS_FALLBACK_INDEX"
        warn "could not find $OS_EDITION/$OS_INST_TYPE in the WIM; using fallback index $idx"
    fi

    disk_gb="${DISK_GB:-$OS_MIN_DISK_GB}"
    local avail
    avail="$(free_gb "$WTL_IMAGES_DIR")"
    if ((avail < 40)); then
        warn "only ${avail} GiB free on $(df -P "$WTL_IMAGES_DIR" | awk 'NR==2{print $6}'); a build needs about 40 GiB"
    else
        log "free disk space: ${avail} GiB"
    fi

    for f in autounattend.xml.tmpl setup.ps1 agent.ps1; do
        [[ -f "$ROOT/guest/$f" ]] || { err "missing $ROOT/guest/$f"; return 2; }
    done

    run_dir="$WTL_RUNS_DIR/build-$img"
    rm -rf "$run_dir"
    stage="$run_dir/stage"
    mkdir -p "$stage/WTL"
    # pick the disk-layout + firmware-specific template; clients (Win10/11) get the Win11 setup
    # bypass block and, for UEFI, a GPT layout. Falls back to the single template if no variant.
    local tmpl="$ROOT/guest/autounattend.xml.tmpl"
    if [[ "$OS_FIRMWARE" == uefi && -f "$ROOT/guest/autounattend-uefi.xml.tmpl" ]]; then
        tmpl="$ROOT/guest/autounattend-uefi.xml.tmpl"
    fi
    # Client (Win10/11) retail media refuses to proceed without a ProductKey; evaluation Server
    # media must NOT get one. Use Microsoft's published generic "Pro" key for clients (installs,
    # does not activate). WTL_PRODUCT_KEY overrides.
    local prodkey=""
    if [[ "$OS_KIND" == client ]]; then
        local pk="${WTL_PRODUCT_KEY:-W269N-WFGWX-YVC9B-4J6C9-T83GX}"
        prodkey="<ProductKey><Key>$pk</Key><WillShowUI>OnError</WillShowUI></ProductKey>"
    fi
    render_template "$tmpl" "$stage/autounattend.xml" \
        "IMAGE_INDEX=$idx" "PASSWORD=$(xml_escape "$WTL_PASSWORD")" "COMPUTERNAME=$cn" \
        "OS=$OS_KEY" "PRODUCTKEY=$prodkey"
    cp "$ROOT/guest/setup.ps1" "$ROOT/guest/agent.ps1" "$stage/WTL/"
    local g
    for g in "$ROOT"/guest/*.ps1; do [[ -f "$g" && ! -e "$stage/WTL/$(basename "$g")" ]] && cp "$g" "$stage/WTL/" ; done
    py - "$stage/WTL/build.json" "$OS_KEY" "$UPDATES" "$WTL_PASSWORD" <<'EOF'
import json, sys
p, os_, upd, pw = sys.argv[1:5]
json.dump({"os": os_, "updates": upd == "true", "password": pw}, open(p, "w"))
EOF
    py "$WTL_LIB/mkiso.py" -o "$run_dir/unattend.iso" -V WTL_BUILD "$stage" \
        || { err "mkiso failed"; return 2; }

    rm -f "$partial"
    "$WTL_QEMU_IMG" create -f qcow2 "$partial" "${disk_gb}G" >/dev/null \
        || { err "qemu-img create failed"; return 2; }

    sock="$(sock_path "$run_dir/qmp.sock" "build-$img")"
    serial="$run_dir/serial.log"
    : >"$serial"
    local vnc="" opts=()
    ((SHOW)) && opts+=(--show) || { vnc="$(pick_vnc_display)"; opts+=(--vnc "$vnc"); }
    # UEFI builds (Win11) need an OVMF firmware + a per-build writable vars file
    if [[ "$OS_FIRMWARE" == uefi ]]; then
        [[ -f "$WTL_OVMF_VARS" ]] || { err "UEFI vars template $WTL_OVMF_VARS not found (install 'ovmf')"; return 2; }
        cp "$WTL_OVMF_VARS" "$run_dir/OVMF_VARS.fd"
        opts+=(--uefi "$run_dir/OVMF_VARS.fd")
    fi
    qemu_cmd --name "wtl-build-$img" --disk "$partial" --cache unsafe \
        --cd "$iso" --cd "$run_dir/unattend.iso" --boot-once-d \
        --serial "$serial" --qmp "$sock" "${opts[@]}"
    start_qemu "$run_dir/qemu.log" || return 2
    CUR_PID="$QEMU_PID" CUR_SOCK="$sock"
    echo "$QEMU_PID" >"$run_dir/qemu.pid"
    log "qemu pid $QEMU_PID; serial log $serial"
    [[ -n "$vnc" ]] && log "watch it: vncviewer 127.0.0.1:$((5900 + vnc))"
    log "waiting for BUILD_DONE (timeout ${TIMEOUT_H}h; Windows Update can take hours)"
    t0=$SECONDS
    rc=0
    run_bg /dev/null "$WTL_PYTHON" "$WTL_LIB/serialwait.py" "$serial" --until BUILD_DONE --fail BUILD_FAIL \
        --timeout "$TIMEOUT_S" --pid "$QEMU_PID" --progress || rc=$?
    case $rc in
        0) ok "guest reported BUILD_DONE after $(( (SECONDS - t0) / 60 )) min; waiting for shutdown" ;;
        1) err "guest reported BUILD_FAIL: $(grep -a 'WTL BUILD_FAIL' "$serial" | tail -1 | tr -d '\r')" ;;
        3) err "timeout after ${TIMEOUT_H}h" ;;
        4) err "qemu exited before BUILD_DONE (see $run_dir/qemu.log)"; sed 's/^/    /' "$run_dir/qemu.log" >&2 || true ;;
        *) err "serialwait failed (rc=$rc)" ;;
    esac
    if ((rc != 0)); then
        if pid_alive "$QEMU_PID"; then
            qmp "$sock" screendump "$run_dir/fail.png" >/dev/null 2>&1 \
                && log "screenshot: $run_dir/fail.png" || true
        fi
        stop_vm "$QEMU_PID" "$sock" 5
        CUR_PID=""
        err "build of $img failed; partial disk kept: $partial ; logs: $run_dir"
        ((rc == 3)) && return 3
        return 2
    fi
    # guest shuts itself down after BUILD_DONE
    if ! wait_pid_exit "$QEMU_PID" 900; then
        warn "guest did not power off within 15 min; sending ACPI powerdown"
        qmp "$sock" powerdown >/dev/null 2>&1 || true
        wait_pid_exit "$QEMU_PID" 300 || stop_vm "$QEMU_PID" "$sock" 10
    fi
    CUR_PID=""
    mv -f "$partial" "$final"
    py - "$WTL_IMAGES_DIR/$img.json" "$OS_KEY" "$iso" "$UPDATES" "$idx" "$disk_gb" "$cn" <<'EOF'
import json, sys, time
p, os_, iso, upd, idx, gb, cn = sys.argv[1:8]
json.dump({"os": os_, "iso": iso, "built_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
           "updates": upd == "true", "image_index": int(idx), "disk_gb": int(gb),
           "computer_name": cn},
          open(p, "w"), indent=1)
EOF
    rm -rf "$stage"
    ok "built $final ($(du -h "$final" | cut -f1)) in $(( (SECONDS - t0) / 60 )) min"
    return 0
}

WORST=0
declare -A RESULT=()
for k in "${KEYS[@]}"; do
    rc=0
    build_one "$k" || rc=$?
    RESULT[$k]=$rc
    ((rc > WORST)) && WORST=$rc
done

if ((${#KEYS[@]} > 1)); then
    printf '\n%-6s %-8s %s\n' OS RESULT IMAGE >&2
    for k in "${KEYS[@]}"; do
        case ${RESULT[$k]} in 0) s=ok ;; 3) s=timeout ;; *) s=failed ;; esac
        printf '%-6s %-8s %s\n' "$k" "$s" "$WTL_IMAGES_DIR/$k.qcow2" >&2
    done
fi
exit "$WORST"
