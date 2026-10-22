# shellcheck shell=bash
# lib/common.sh - shared helpers for win-test-lab host scripts.
# Source it from a script that has `set -euo pipefail`; everything here is safe under it.

# ---------------------------------------------------------------- paths
WTL_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$WTL_LIB/.." && pwd)"
export ROOT WTL_LIB

# ---------------------------------------------------------------- env defaults
: "${WTL_ISO_DIR:=$ROOT/iso-files}"
: "${WTL_IMAGES_DIR:=$ROOT/images}"
: "${WTL_RUNS_DIR:=$ROOT/runs}"
: "${WTL_RESULTS_DIR:=$ROOT/results}"
: "${WTL_QEMU:=qemu-system-x86_64}"
: "${WTL_QEMU_IMG:=qemu-img}"
: "${WTL_NO_KVM:=}"
: "${WTL_MEM:=4096}"
: "${WTL_CPUS:=2}"
: "${WTL_PASSWORD:=WinTest!2026}"
: "${WTL_PYTHON:=python3}"
export WTL_ISO_DIR WTL_IMAGES_DIR WTL_RUNS_DIR WTL_RESULTS_DIR WTL_QEMU WTL_QEMU_IMG \
       WTL_NO_KVM WTL_MEM WTL_CPUS WTL_PASSWORD WTL_PYTHON

OSLIST_CONF="$WTL_LIB/oslist.conf"

# ---------------------------------------------------------------- logging
_wtl_ts() { date '+%H:%M:%S'; }
if [[ -t 2 ]]; then
    _C_RED=$'\e[31m'; _C_YEL=$'\e[33m'; _C_GRN=$'\e[32m'; _C_DIM=$'\e[2m'; _C_OFF=$'\e[0m'
else
    _C_RED=; _C_YEL=; _C_GRN=; _C_DIM=; _C_OFF=
fi
log()  { printf '%s[%s]%s %s\n' "$_C_DIM" "$(_wtl_ts)" "$_C_OFF" "$*" >&2; }
ok()   { printf '%s[%s]%s %s%s%s\n' "$_C_DIM" "$(_wtl_ts)" "$_C_OFF" "$_C_GRN" "$*" "$_C_OFF" >&2; }
warn() { printf '%s[%s]%s %sWARNING:%s %s\n' "$_C_DIM" "$(_wtl_ts)" "$_C_OFF" "$_C_YEL" "$_C_OFF" "$*" >&2; }
err()  { printf '%s[%s]%s %sERROR:%s %s\n' "$_C_DIM" "$(_wtl_ts)" "$_C_OFF" "$_C_RED" "$_C_OFF" "$*" >&2; }
# die MSG [CODE]   (default exit code 2 = setup error)
die()  { err "$1"; exit "${2:-2}"; }

py() { "$WTL_PYTHON" "$@"; }

# ---------------------------------------------------------------- OS table
IMAGES_CONF="$ROOT/images-list.conf"

# os_normalize NAME -> canonical key (aliases: 2012r2, ws2019, server2019, windows11 ...)
os_normalize() {
    local k
    k="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
    case "$k" in
        2012r2|2012-r2|2012_r2|win2012r2|windowsserver2012r2) k=2012 ;;
        win2016|server2016|ws2016) k=2016 ;;
        win2019|server2019|ws2019) k=2019 ;;
        win2022|server2022|ws2022) k=2022 ;;
        win2025|server2025|ws2025) k=2025 ;;
        windows10|windows_10) k=win10 ;;
        windows11|windows_11) k=win11 ;;
    esac
    # variant aliases (win11_en_x64, win10_de_arm64, ...) fold to the base client key
    case "$k" in
        win10_*) k=win10 ;;
        win11_*) k=win11 ;;
    esac
    printf '%s\n' "$k"
}

# os_keys -> every key in oslist.conf, one per line
os_keys() {
    awk '!/^[[:space:]]*(#|$)/ {print $1}' "$OSLIST_CONF"
}

# os_build KEY -> expected major build number (from oslist.conf)
os_build() { os_lookup "$1" >/dev/null 2>&1 && printf '%s\n' "$OS_BUILD"; }

# os_lookup KEY -> sets OS_KEY OS_BUILD OS_EDITION OS_INST_TYPE OS_KIND OS_FIRMWARE
#                  OS_FALLBACK_INDEX OS_MIN_DISK_GB OS_DISPLAY ; returns 1 if unknown.
# OS_KIND is "server" (can be a DC) or "client" (Win10/11); OS_FIRMWARE "bios" or "uefi".
os_lookup() {
    local want line key bld ed it kind fw fb md disp
    want="$(os_normalize "$1")"
    while IFS= read -r line || [[ -n "$line" ]]; do
        line="${line%%#*}"
        [[ -z "${line//[[:space:]]/}" ]] && continue
        read -r key bld ed it kind fw fb md disp <<<"$line"
        if [[ "$key" == "$want" ]]; then
            OS_KEY="$key"; OS_BUILD="$bld"; OS_EDITION="$ed"; OS_INST_TYPE="${it//_/ }"
            OS_KIND="$kind"; OS_FIRMWARE="$fw"
            OS_FALLBACK_INDEX="$fb"; OS_MIN_DISK_GB="$md"; OS_DISPLAY="$disp"
            return 0
        fi
    done <"$OSLIST_CONF"
    return 1
}

# iso_build ISO -> prints major build of WIM image 1; non-zero if no WIM / unreadable
iso_build() { py "$WTL_LIB/isoinfo.py" build "$1" 2>/dev/null; }

# catalog_iso KEY -> the ISO filename mapped to KEY in images-list.conf (empty if none)
catalog_iso() {
    [[ -f "$IMAGES_CONF" ]] || return 1
    local want line k v
    want="$(os_normalize "$1")"
    while IFS= read -r line || [[ -n "$line" ]]; do
        line="${line%%#*}"
        [[ "$line" == *=* ]] || continue
        k="${line%%=*}"; v="${line#*=}"
        # trim
        k="${k#"${k%%[![:space:]]*}"}"; k="${k%"${k##*[![:space:]]}"}"
        v="${v#"${v%%[![:space:]]*}"}"; v="${v%"${v##*[![:space:]]}"}"
        if [[ "$k" == "$want" ]]; then printf '%s\n' "$v"; return 0; fi
    done <"$IMAGES_CONF"
    return 1
}

# newest-first listing of existing files given as args
_newest_first() {
    local f
    for f in "$@"; do [[ -f "$f" ]] && printf '%s\t%s\n' "$(stat -c %Y "$f")" "$f"; done \
        | sort -rn | cut -f2-
}

# find_iso KEY -> prints the path of the best ISO for KEY; returns 1 if none.
#  1) the images-list.conf entry for KEY (path relative to WTL_ISO_DIR unless absolute),
#     accepted only if its WIM build matches the OS table.
#  2) fallback: every *.iso in WTL_ISO_DIR matched by WIM <BUILD>; ISOs without WIM skipped.
find_iso() {
    os_lookup "$1" || return 1
    local f b cat
    if cat="$(catalog_iso "$1")" && [[ -n "$cat" ]]; then
        [[ "$cat" == /* ]] || cat="$WTL_ISO_DIR/$cat"
        if [[ -f "$cat" ]]; then
            b="$(iso_build "$cat")" || b=""
            if [[ "$b" == "$OS_BUILD" ]]; then printf '%s\n' "$cat"; return 0; fi
            [[ -n "$b" ]] && warn "$1: catalog ISO $cat is build $b, expected $OS_BUILD; ignoring it"
        else
            warn "$1: catalog lists $cat but it is missing"
        fi
    fi
    local cands=()
    shopt -s nullglob nocaseglob
    cands=( "$WTL_ISO_DIR"/*.iso )
    shopt -u nullglob nocaseglob
    ((${#cands[@]})) || return 1
    while IFS= read -r f; do
        b="$(iso_build "$f")" || continue
        if [[ "$b" == "$OS_BUILD" ]]; then printf '%s\n' "$f"; return 0; fi
    done < <(_newest_first "${cands[@]}")
    return 1
}

# ---------------------------------------------------------------- ports / vnc
# port_free PORT -> 0 if 127.0.0.1:PORT can be bound
port_free() {
    py - "$1" <<'EOF'
import socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
try:
    s.bind(("127.0.0.1", int(sys.argv[1])))
except OSError:
    sys.exit(1)
finally:
    s.close()
EOF
}

# free_port -> prints an ephemeral free TCP port on 127.0.0.1
free_port() {
    py -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

# pick_vnc_display -> prints N (0..99) such that 127.0.0.1:(5900+N) is free
pick_vnc_display() {
    py - <<'EOF'
import socket, sys
for n in list(range(10, 100)) + list(range(0, 10)):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", 5900 + n))
    except OSError:
        continue
    finally:
        s.close()
    print(n)
    sys.exit(0)
sys.exit(1)
EOF
}

# ---------------------------------------------------------------- misc helpers
# free_gb DIR -> integer GiB available on the filesystem holding DIR
free_gb() {
    local d="$1"
    while [[ ! -d "$d" ]]; do d="$(dirname "$d")"; done
    df -Pk "$d" | awk 'NR==2 {printf "%d\n", $4/1048576}'
}

# pid_alive PID
pid_alive() {
    [[ -n "${1:-}" ]] || return 1
    kill -0 "$1" 2>/dev/null || return 1
    local st
    st="$(awk '{print $3}' "/proc/$1/stat" 2>/dev/null || true)"
    [[ "$st" != Z && "$st" != X ]]
}

# wait_pid_exit PID SECONDS -> 0 if exited within SECONDS
wait_pid_exit() {
    local pid="$1" t="$2" i=0
    while pid_alive "$pid"; do
        ((i >= t * 5)) && return 1
        sleep 0.2; i=$((i + 1))
    done
    wait "$pid" 2>/dev/null || true
    return 0
}

# qmp SOCK CMD [ARGS...]  (never fails the caller)
qmp() { py "$WTL_LIB/qmp.py" --timeout 10 "$@" || return $?; }

# stop_vm PID SOCK [GRACE_S] : QMP quit, then TERM, then KILL
stop_vm() {
    local pid="$1" sock="$2" grace="${3:-10}"
    pid_alive "$pid" || { wait "$pid" 2>/dev/null || true; return 0; }
    if [[ -S "$sock" ]]; then qmp "$sock" quit >/dev/null 2>&1 || true; fi
    wait_pid_exit "$pid" "$grace" && return 0
    kill -TERM "$pid" 2>/dev/null || true
    wait_pid_exit "$pid" 5 && return 0
    kill -KILL "$pid" 2>/dev/null || true
    wait_pid_exit "$pid" 5 || true
}

# sock_path PREFERRED NAME -> a unix socket path short enough (<= 100 chars)
sock_path() {
    if ((${#1} <= 100)); then printf '%s\n' "$1"; return; fi
    printf '%s/wtl-%s-%s.sock\n' "${XDG_RUNTIME_DIR:-/tmp}" "$2" "$$"
}

# json_get FILE KEY
json_get() { py -c 'import json,sys; print(json.load(open(sys.argv[1])).get(sys.argv[2],""))' "$1" "$2"; }

# ---------------------------------------------------------------- qemu command line
# qemu_cmd [options]  -> fills the global array QEMU_CMD
#   --name N  --disk QCOW2  --cache MODE  --cd ISO (repeatable, IDE order)
#   --serial FILE  --qmp SOCK  --fwd HOST:GUEST (repeatable)  --boot-once-d
#   --show  --vnc N  --pidfile F
#   --lan server:PORT|client:PORT   second NIC on a private VM-to-VM LAN (TCP stream
#                                   on 127.0.0.1:PORT; the client end reconnects)
#   --lan-mac MAC  --no-usernet     (no user-mode NAT NIC, i.e. only the private LAN)
#   --uefi VARS     boot UEFI: q35 + OVMF firmware, writable vars file VARS (for Win11)
#   --tpm DIR       add an emulated TPM 2.0 via swtpm in DIR (Win11; needs swtpm installed)
# Machine (default): i440fx ("pc") + IDE CD-ROMs, disk on ich9-ahci (in-box storahci on
# 2012 R2+), e1000 NIC (in-box on 2012 R2+; WTL_NIC overrides), std VGA, usb-tablet,
# localtime RTC. With --uefi the machine is q35 and OVMF is loaded via pflash.
WTL_OVMF_CODE="${WTL_OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}"
WTL_OVMF_VARS="${WTL_OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}"
QEMU_CMD=()
qemu_cmd() {
    local name=wtl disk="" cache=writeback serial="" qmpsock="" bootd=0 show=0 vnc="" pidfile=""
    local lan="" lanmac="" usernet=1 uefi="" tpmdir="" noshutdown=0
    local cds=() fwds=()
    while (($#)); do
        case "$1" in
            --name) name="$2"; shift 2 ;;
            --disk) disk="$2"; shift 2 ;;
            --cache) cache="$2"; shift 2 ;;
            --cd) cds+=("$2"); shift 2 ;;
            --serial) serial="$2"; shift 2 ;;
            --qmp) qmpsock="$2"; shift 2 ;;
            --fwd) fwds+=("$2"); shift 2 ;;
            --boot-once-d) bootd=1; shift ;;
            --show) show=1; shift ;;
            --vnc) vnc="$2"; shift 2 ;;
            --pidfile) pidfile="$2"; shift 2 ;;
            --lan) lan="$2"; shift 2 ;;
            --lan-mac) lanmac="$2"; shift 2 ;;
            --no-usernet) usernet=0; shift ;;
            --uefi) uefi="$2"; shift 2 ;;
            --tpm) tpmdir="$2"; shift 2 ;;
            --no-shutdown) noshutdown=1; shift ;;
            *) die "qemu_cmd: bad option $1" ;;
        esac
    done
    local nic="${WTL_NIC:-e1000}"
    local machine=pc
    [[ -n "$uefi" ]] && machine=q35
    QEMU_CMD=("$WTL_QEMU" -name "$name" -machine "$machine" -m "$WTL_MEM" -smp "$WTL_CPUS")
    if [[ "$WTL_NO_KVM" == 1 ]]; then
        QEMU_CMD+=(-cpu max)
    else
        QEMU_CMD+=(-enable-kvm -cpu host,hv_relaxed,hv_spinlocks=0x1fff,hv_vapic,hv_time)
    fi
    if [[ -n "$uefi" ]]; then
        [[ -f "$WTL_OVMF_CODE" ]] || die "UEFI firmware $WTL_OVMF_CODE not found (install the 'ovmf' package or set WTL_OVMF_CODE)"
        QEMU_CMD+=(-drive "if=pflash,format=raw,readonly=on,file=$(_qesc "$WTL_OVMF_CODE")")
        QEMU_CMD+=(-drive "if=pflash,format=raw,file=$(_qesc "$uefi")")
    fi
    if [[ -n "$tpmdir" ]]; then
        QEMU_CMD+=(-chardev "socket,id=chrtpm,path=$tpmdir/swtpm.sock")
        QEMU_CMD+=(-tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0)
    fi
    QEMU_CMD+=(-rtc base=localtime -vga std -usb -device usb-tablet)
    QEMU_CMD+=(-device ich9-ahci,id=ahci)
    QEMU_CMD+=(-drive "id=disk0,if=none,format=qcow2,file=$(_qesc "$disk"),cache=$cache,discard=unmap,detect-zeroes=unmap")
    QEMU_CMD+=(-device ide-hd,drive=disk0,bus=ahci.0)
    local i=0 c
    for c in "${cds[@]+"${cds[@]}"}"; do
        QEMU_CMD+=(-drive "file=$(_qesc "$c"),media=cdrom,if=ide,index=$i,readonly=on")
        i=$((i + 1))
    done
    if ((usernet)); then
        local net="user,id=net0"
        for c in "${fwds[@]+"${fwds[@]}"}"; do
            net+=",hostfwd=tcp:127.0.0.1:${c%%:*}-:${c##*:}"
        done
        QEMU_CMD+=(-netdev "$net" -device "$nic,netdev=net0")
    fi
    if [[ -n "$lan" ]]; then
        local addr="addr.type=inet,addr.host=127.0.0.1,addr.port=${lan##*:}"
        case "${lan%%:*}" in
            server) QEMU_CMD+=(-netdev "stream,id=lan0,server=on,$addr") ;;
            client) QEMU_CMD+=(-netdev "stream,id=lan0,server=off,reconnect-ms=1000,$addr") ;;
            *) die "qemu_cmd: bad --lan $lan" ;;
        esac
        QEMU_CMD+=(-device "$nic,netdev=lan0${lanmac:+,mac=$lanmac}")
    fi
    [[ -n "$serial" ]] && QEMU_CMD+=(-serial "file:$serial")
    [[ -n "$qmpsock" ]] && QEMU_CMD+=(-qmp "unix:$qmpsock,server=on,wait=off")
    if ((bootd)); then QEMU_CMD+=(-boot once=d); else QEMU_CMD+=(-boot order=c); fi
    if ((show)); then
        QEMU_CMD+=(-display gtk)
    else
        QEMU_CMD+=(-display none)
        [[ -n "$vnc" ]] && QEMU_CMD+=(-vnc "127.0.0.1:$vnc")
    fi
    [[ -n "$pidfile" ]] && QEMU_CMD+=(-pidfile "$pidfile")
    # keep QEMU alive if the guest powers off (some clients do that on a reboot request); the host
    # then issues system_reset to continue. A guest reset still reboots normally.
    ((noshutdown)) && QEMU_CMD+=(-no-shutdown)
    return 0
}
# qemu -drive option values escape ',' as ',,'
_qesc() { printf '%s' "${1//,/,,}"; }

# start_qemu LOGFILE -> starts "${QEMU_CMD[@]}" in the background (own session so
# a terminal Ctrl-C reaches only us; we stop it via QMP). Sets QEMU_PID.
QEMU_PID=""
start_qemu() {
    local logf="$1"
    printf '%q ' "${QEMU_CMD[@]}" >"$logf.cmdline"; echo >>"$logf.cmdline"
    if command -v setsid >/dev/null 2>&1; then
        setsid "${QEMU_CMD[@]}" </dev/null >"$logf" 2>&1 &
    else
        "${QEMU_CMD[@]}" </dev/null >"$logf" 2>&1 &
    fi
    QEMU_PID=$!
    sleep 0.5
    if ! pid_alive "$QEMU_PID"; then
        wait "$QEMU_PID" 2>/dev/null || true
        err "qemu failed to start:"
        sed 's/^/    /' "$logf" >&2 || true
        QEMU_PID=""
        return 1
    fi
    return 0
}

# run_bg OUTFILE CMD... : run CMD in the background with stdout -> OUTFILE and `wait`
# for it, so trapped signals (Ctrl-C, TERM) are handled immediately instead of after
# CMD finishes. Returns CMD's exit status. The child pid is in BG_PID while it runs.
BG_PID=""
run_bg() {
    local out="$1"; shift
    "$@" >"$out" &
    BG_PID=$!
    local rc=0
    wait "$BG_PID" || rc=$?
    BG_PID=""
    return "$rc"
}
kill_bg() { [[ -n "${BG_PID:-}" ]] && kill "$BG_PID" 2>/dev/null; BG_PID=""; return 0; }

# check_tools : ensure python3 / qemu / qemu-img are runnable
check_tools() {
    command -v "$WTL_PYTHON" >/dev/null 2>&1 || die "python3 not found (run ./setup-host.sh)"
    local t
    for t in "$@"; do
        command -v "$t" >/dev/null 2>&1 || die "'$t' not found (run ./setup-host.sh or set WTL_QEMU / WTL_QEMU_IMG)"
    done
    if [[ "$WTL_NO_KVM" != 1 && ! -w /dev/kvm ]]; then
        die "/dev/kvm missing or not writable (run ./setup-host.sh; or WTL_NO_KVM=1 for very slow TCG)"
    fi
}
