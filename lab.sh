#!/usr/bin/env bash
# lab.sh - Active Directory lab: a domain controller + a domain-joined client, on QEMU/KVM.
#
#   lab run CONFIG [CONFIG...]        run a config file: boot the lab it names, mount host
#                                     folders as drives, run its scripts as admin / as the student.
#       --show           open the VM windows (default: headless, VNC addresses printed)
#       --wait S         seconds a step may run before it counts as "still running" and is
#                        left running (default 60; overrides the config's own 'wait')
#
#   A CONFIG is a key=value file (see examples/configs/, `lab help-config`):
#       version = 2019                      # the lab (labs/<version>.conf) to boot
#       mount   = /host/folder:X, /other:Z  # host folder -> read-only drive letter in the guest
#       run_as_admin   = X:\\install.bat     # a step run as the lab's root (the SYSTEM account)
#       run_as_student = Z:\\run.bat          # a step run as LAB\\student
#   Steps run in the order written (run_as_admin / run_as_student lines may be interleaved).
#   Host folders are mounted read-only (a snapshot taken at launch). Paths in the config are
#   relative to the config file. LAB-affecting commands below also accept a CONFIG (it names the lab).
#
#   lab restore LAB      stop the VMs and throw away everything since the 0 state (~1 s)
#   lab up LAB           boot the lab from its 0 state without deploying anything
#   lab stop LAB         power the VMs off (changes kept until the next run / restore)
#   lab status LAB
#   lab prepare LAB      build the 0 state (run does this automatically the first time)
#   lab rebuild LAB      delete the 0 state; the next run builds a new domain (Windows stays)
#
#   LAB = labs/LAB.conf (e.g. 2012, 2019). The 0 state = Windows installed, domain created,
#   client joined, LAB\student logging on automatically, both shut down cleanly. It is frozen
#   (labs/state/LAB/*-0.qcow2); runs write only to small throwaway overlays on top of it.
# Exit: 0 all steps passed, 1 a step failed, 2 lab error, 3 timeout.
set -euo pipefail
# shellcheck source=lib/common.sh
source "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/lib/common.sh"

usage() {
    sed -n '2,27p' "$(readlink -f "$0")" | sed 's/^# \{0,1\}//'
    exit "${1:-2}"
}

CMD="${1:-}"
[[ "$CMD" == -h || "$CMD" == --help || "$CMD" == help ]] && usage 0
if [[ "$CMD" == help-config ]]; then sed -n '2,34p' "$WTL_LIB/labconf.py" 2>/dev/null | sed 's/^"""//; s/"""$//'; exit 0; fi
LAB="${2:-}"
[[ -n "$CMD" && -n "$LAB" ]] || usage 2
shift 2

# A "run" always names a CONFIG file (lab run my.conf [other.conf ...]). For LAB-affecting
# commands (restore/up/stop/status/prepare/rebuild) the target may also be a CONFIG, from which
# we read the version. Detect a config = an existing file that is not a bare lab name, parse it,
# and let its version stand in for LAB.
CONFIG="" CONFIGS=()
RUN_MOUNTS_JSON="[]" RUN_STEPS_JSON="[]" RUN_NAME=""
load_config() {  # PATH... -> sets LAB + RUN_* from the merged configs
    CONFIGS=("$@")
    local abs=() p
    for p in "$@"; do abs+=("$(readlink -f "$p")"); done
    CONFIG="${abs[0]}"   # first config; mounts are already resolved to absolute by labconf
    local js; js="$(py "$WTL_LIB/labconf.py" json "${abs[@]}")" || die "config error (see above)"
    LAB="$(py -c 'import json,sys;print(json.load(sys.stdin)["version"])' <<<"$js")"
    RUN_NAME="$(py -c 'import json,sys;print(json.load(sys.stdin)["name"])' <<<"$js")"
    RUN_MOUNTS_JSON="$(py -c 'import json,sys;print(json.dumps(json.load(sys.stdin)["mounts"]))' <<<"$js")"
    RUN_STEPS_JSON="$(py -c 'import json,sys;print(json.dumps(json.load(sys.stdin)["steps"]))' <<<"$js")"
    WAIT_S="$(py -c 'import json,sys;print(json.load(sys.stdin)["wait"])' <<<"$js")"
    TIMEOUT_MIN="$(py -c 'import json,sys;print(json.load(sys.stdin)["timeout_min"])' <<<"$js")"
}
SHOW=0 UPDATES=0 WAIT_S=60 WAIT_OVERRIDE="" TIMEOUT_MIN=120
# is X a config file (not just a bare lab name like 2019)?
is_config() { [[ -f "$1" && ( "$1" == *.conf || "$1" == */* ) ]] && ! [[ -f "$ROOT/labs/$1.conf" ]]; }

if [[ "$CMD" == run ]]; then
    is_config "$LAB" || { err "usage: lab run CONFIG [CONFIG...]  (CONFIG is a .conf file; see 'lab help-config')"; exit 2; }
    cfgs=("$LAB")
    while (($#)) && is_config "$1"; do cfgs+=("$1"); shift; done
    load_config "${cfgs[@]}"
elif is_config "$LAB"; then
    cfgs=("$LAB")
    while (($#)) && is_config "$1"; do cfgs+=("$1"); shift; done
    load_config "${cfgs[@]}"   # restore/up/status/... given config(s): use the merged version
fi
while (($#)); do
    case "$1" in
        --show) SHOW=1; shift ;;
        --updates) UPDATES=1; shift ;;
        --wait) WAIT_OVERRIDE="${2:?}"; shift 2 ;;
        --timeout-min) TIMEOUT_MIN="${2:?}"; shift 2 ;;
        -h|--help) usage 0 ;;
        *) err "unknown option $1"; usage 2 ;;
    esac
done
# --wait overrides the config's own 'wait' (WAIT_S came from the config if one was loaded)
[[ -n "$WAIT_OVERRIDE" ]] && WAIT_S="$WAIT_OVERRIDE"
[[ "$WAIT_S" =~ ^[0-9]+$ && "$WAIT_S" -gt 0 ]] || die "--wait must be a positive integer"
[[ "$TIMEOUT_MIN" =~ ^[0-9]+$ ]] || die "--timeout-min must be an integer"

# ------------------------------------------------------------------ config
CONF="$ROOT/labs/$LAB.conf"
[[ "$LAB" =~ ^[A-Za-z0-9._-]+$ && -f "$CONF" ]] || die "no lab config $CONF (labs: $(cd "$ROOT/labs" && ls *.conf 2>/dev/null | sed 's/\.conf$//' | tr '\n' ' '))"
LAB_OS="" LAB_CLIENT_OS="" LAB_DOMAIN="" LAB_NETBIOS="" LAB_DC_NAME="" LAB_CLIENT_NAME=""
LAB_NET="" LAB_PREFIX=24 LAB_LAN_PORT="" LAB_ADMIN_USER="" LAB_ADMIN_USER_PASSWORD=""
LAB_USER="" LAB_USER_PASSWORD="" LAB_DC_MEM=3072 LAB_CLIENT_MEM=4096
# shellcheck disable=SC1090
source "$CONF"
: "${LAB_CLIENT_OS:=$LAB_OS}"
for v in LAB_OS LAB_DOMAIN LAB_NETBIOS LAB_DC_NAME LAB_CLIENT_NAME LAB_NET LAB_LAN_PORT LAB_USER LAB_USER_PASSWORD; do
    [[ -n "${!v}" ]] || die "$CONF: $v is not set"
done
os_lookup "$LAB_OS" || die "$CONF: unknown LAB_OS $LAB_OS"
DC_OS="$OS_KEY" DC_FIRMWARE="$OS_FIRMWARE" DC_KIND="$OS_KIND"
[[ "$DC_KIND" == server ]] || die "$CONF: LAB_OS $LAB_OS is a client OS and cannot be the domain controller (use a Windows Server for LAB_OS)"
os_lookup "$LAB_CLIENT_OS" || die "$CONF: unknown LAB_CLIENT_OS $LAB_CLIENT_OS"
CL_OS="$OS_KEY" CL_FIRMWARE="$OS_FIRMWARE" CL_KIND="$OS_KIND"
DC_IP="$LAB_NET.10" CL_IP="$LAB_NET.20"
DC_MAC=52:54:00:4c:41:10 CL_MAC=52:54:00:4c:41:20

DC_BASE="$WTL_IMAGES_DIR/$LAB-dc.qcow2" CL_BASE="$WTL_IMAGES_DIR/$LAB-client.qcow2"
STATE="$ROOT/labs/state/$LAB"
ZERO_DONE="$STATE/0-state.json"      # written when the 0 state is complete
mkdir -p "$STATE"

vm_pid() { local p; p="$(cat "$STATE/$1.pid" 2>/dev/null || true)"; pid_alive "$p" && echo "$p"; return 0; }
vm_sock() { sock_path "$STATE/$1.qmp" "lab-$LAB-$1"; }
zero_disk() { echo "$STATE/$1-0.qcow2"; }       # frozen 0 state
run_disk() { echo "$STATE/$1.qcow2"; }          # throwaway overlay of the current run

# ------------------------------------------------------------------ stop / restore / status
# stop_vms hard|soft : hard = QMP quit (instant; for throwaway runs), soft = ACPI power off
stop_vms() {
    local how="$1" vm pid i
    for vm in client dc; do
        pid="$(vm_pid "$vm")"
        [[ -n "$pid" ]] || { rm -f "$STATE/$vm.pid"; continue; }
        if [[ $how == hard ]]; then
            qmp "$(vm_sock "$vm")" quit >/dev/null 2>&1 || kill "$pid" 2>/dev/null || true
        else
            qmp "$(vm_sock "$vm")" powerdown >/dev/null 2>&1 || true
        fi
    done
    for vm in client dc; do
        pid="$(vm_pid "$vm")"
        [[ -n "$pid" ]] || continue
        # under -no-shutdown the guest powering off does NOT end QEMU; it reaches QMP status
        # "shutdown". Wait for either that or the pid to die, then quit QEMU ourselves.
        i=0
        while pid_alive "$pid" && ((i < 900)); do
            [[ "$(qmp "$(vm_sock "$vm")" status 2>/dev/null | tr -d '\r\n')" == shutdown ]] && break
            sleep 0.2; i=$((i + 1))
        done
        if pid_alive "$pid"; then
            if ((i >= 900)); then warn "$vm did not power off in 3 min, stopping it hard"; fi
            qmp "$(vm_sock "$vm")" quit >/dev/null 2>&1 || kill -KILL "$pid" 2>/dev/null || true
            sleep 1
        fi
        rm -f "$STATE/$vm.pid"
    done
}

restore() {
    stop_vms hard
    rm -f "$(run_disk dc)" "$(run_disk client)"
}

# freeze_off VM : cleanly power a VM off for the 0-state freeze. Under -no-shutdown the guest
# powering off reaches QMP status "shutdown" without ending QEMU; wait for that (or pid exit),
# then quit QEMU. Returns non-zero if it never powered off.
freeze_off() {
    local vm="$1" pid i=0
    pid="$(cat "$STATE/$vm.pid" 2>/dev/null || true)"
    [[ -n "$pid" ]] || return 0
    qmp "$(vm_sock "$vm")" powerdown >/dev/null 2>&1 || true
    while pid_alive "$pid" && ((i < 1800)); do
        [[ "$(qmp "$(vm_sock "$vm")" status 2>/dev/null | tr -d '\r\n')" == shutdown ]] && break
        if ((i == 300)); then  # ~60s: a user may be logged on; a second press shuts it down
            qmp "$(vm_sock "$vm")" powerdown >/dev/null 2>&1 || true
        fi
        sleep 0.2; i=$((i + 1))
    done
    pid_alive "$pid" || return 0
    [[ "$(qmp "$(vm_sock "$vm")" status 2>/dev/null | tr -d '\r\n')" == shutdown ]] || { ((i >= 1800)) && return 1; }
    qmp "$(vm_sock "$vm")" quit >/dev/null 2>&1 || kill "$pid" 2>/dev/null || true
    local j=0
    while pid_alive "$pid" && ((j < 50)); do sleep 0.2; j=$((j + 1)); done
    return 0
}

lab_status() {
    local vm pid f
    echo "lab $LAB ($CONF): domain $LAB_DOMAIN, DC $LAB_DC_NAME $DC_IP, client $LAB_CLIENT_NAME $CL_IP"
    if [[ -f "$ZERO_DONE" ]]; then
        echo "  0 state: ready ($(json_get "$ZERO_DONE" built_at))"
    else
        echo "  0 state: not built yet (lab prepare $LAB, or just lab run ...)"
    fi
    for f in "$DC_BASE" "$CL_BASE" "$(zero_disk dc)" "$(zero_disk client)" "$(run_disk dc)" "$(run_disk client)"; do
        [[ -f "$f" ]] && echo "  disk  $(basename "$f")  $(du -h "$f" | cut -f1)"
    done
    for vm in dc client; do
        pid="$(vm_pid "$vm")"
        if [[ -n "$pid" ]]; then echo "  $vm: running (pid $pid) $(cat "$STATE/$vm.vnc" 2>/dev/null || true)"; else echo "  $vm: off"; fi
    done
    [[ -L "$STATE/last" ]] && echo "  last run: $(readlink -f "$STATE/last")"
    return 0
}

case "$CMD" in
    stop|down) stop_vms soft; ok "lab $LAB is off"; exit 0 ;;
    restore) restore; ok "lab $LAB is back at its 0 state (VMs off; next run/up starts clean)"; exit 0 ;;
    status) lab_status; exit 0 ;;
    rebuild|reset)
        restore
        rm -f "$(zero_disk dc)" "$(zero_disk client)" "$ZERO_DONE"
        ok "lab $LAB: 0 state deleted; the next run builds a new domain (Windows installs kept)"
        exit 0 ;;
    run|up|prepare) ;;
    *) err "unknown command $CMD"; usage 2 ;;
esac

# ------------------------------------------------------------------ checks
check_tools "$WTL_QEMU" "$WTL_QEMU_IMG"

DEADLINE=$((SECONDS + TIMEOUT_MIN * 60))
RUN_ID="$(date +%Y%m%d-%H%M%S)"
on_signal() {
    trap - INT TERM
    echo >&2
    kill_bg
    warn "interrupted; VMs may still be running (lab status $LAB / lab restore $LAB)"
    exit 2
}
trap on_signal INT TERM

# ------------------------------------------------------------------ helpers
# MOUNT_ISOS holds the labelled per-mount discs built for the client (set by make_mount_isos).
MOUNT_ISOS=()
# make_mount_isos : one read-only ISO per config mount, volume label WTL<LETTER>, from the host
# folder (resolved relative to the config). Fills MOUNT_ISOS with their paths.
make_mount_isos() {
    MOUNT_ISOS=()
    local n; n="$(py -c 'import json,sys;print(len(json.loads(sys.argv[1])))' "$RUN_MOUNTS_JSON")"
    local i
    for ((i = 0; i < n; i++)); do
        local letter host src iso
        letter="$(py -c 'import json,sys;print(json.loads(sys.argv[1])[int(sys.argv[2])]["letter"])' "$RUN_MOUNTS_JSON" "$i")"
        host="$(py -c 'import json,sys;print(json.loads(sys.argv[1])[int(sys.argv[2])]["host"])' "$RUN_MOUNTS_JSON" "$i")"
        src="$host"   # labconf.py already resolved mount hosts to absolute paths
        [[ -d "$src" ]] || die "mount source is not a directory: $src (drive $letter:)"
        iso="$STATE/mount-$letter.iso"
        py "$WTL_LIB/mkiso.py" -o "$iso" -V "WTL$letter" "$src" || die "mkiso for drive $letter: failed"
        MOUNT_ISOS+=("$iso")
        log "mount $letter: <- $src ($(du -sh "$src" 2>/dev/null | cut -f1))"
    done
}

# make_cd VM ROLE OUT [shutdown_when_ready] : LAB\lab.ps1 + LAB\lab.json (+ the run's steps)
make_cd() {
    local vm="$1" role="$2" out="$3" sdown="${4:-0}" st="$STATE/cd-$vm" ip mac
    ip="$DC_IP" mac="$DC_MAC"
    [[ $role == client ]] && ip="$CL_IP" mac="$CL_MAC"
    rm -rf "$st"; mkdir -p "$st/LAB"
    cp "$ROOT/guest/lab.ps1" "$ROOT/guest/labsvc.ps1" "$st/LAB/"
    local run_json="null"
    if [[ $role == client && $CMD == run && "$sdown" == 0 && -n "$CONFIG" ]]; then
        run_json="$(py -c 'import json,sys
print(json.dumps({"mounts": json.loads(sys.argv[1]), "steps": json.loads(sys.argv[2]),
                   "wait": int(sys.argv[3])}))' \
            "$RUN_MOUNTS_JSON" "$RUN_STEPS_JSON" "$WAIT_S")"
    fi
    py - "$st/LAB/lab.json" "$role" "$ip" "$DC_IP" "$LAB_PREFIX" "$LAB_DOMAIN" "$LAB_NETBIOS" \
        "$WTL_PASSWORD" "$LAB_ADMIN_USER" "$LAB_ADMIN_USER_PASSWORD" "$LAB_USER" "$LAB_USER_PASSWORD" \
        "$mac" "$sdown" "$run_json" <<'EOF'
import json, sys
(p, role, ip, dc_ip, prefix, domain, netbios, admin_pw, au, aupw, user, upw, mac, sdown, run) = sys.argv[1:16]
users = [{"name": user, "password": upw, "groups": []}]
if au:
    users.append({"name": au, "password": aupw, "groups": ["Domain Admins"]})
json.dump({"role": role, "ip": ip, "prefix": int(prefix), "dc_ip": dc_ip, "lan_mac": mac,
           "domain": domain, "netbios": netbios, "admin_password": admin_pw,
           "users": users, "logon_user": user, "logon_password": upw,
           "shutdown_when_ready": sdown == "1", "run": json.loads(run)},
          open(p, "w"), indent=1)
EOF
    py "$WTL_LIB/mkiso.py" -o "$out" -V WTL_LAB "$st" || die "mkiso $vm failed"
    rm -rf "$st"
}

# start_vm VM DISK CD [extra CDs...] -- qemu_cmd-args...   (DC first: its end of the lab LAN listens)
# Everything up to a literal "--" is a CD (the control disc, then any mount discs).
start_vm() {
    local vm="$1" disk="$2"; shift 2
    local cds=() mem="$LAB_DC_MEM" vnc="" opts=() sock
    while (($#)); do [[ "$1" == "--" ]] && { shift; break; }; cds+=("$1"); shift; done
    [[ $vm == client ]] && mem="$LAB_CLIENT_MEM"
    sock="$(vm_sock "$vm")"
    rm -f "$sock"
    : >"$STATE/$vm.serial.log"
    if ((SHOW)); then opts+=(--show); else vnc="$(pick_vnc_display)"; opts+=(--vnc "$vnc"); fi
    local cdargs=() c
    for c in "${cds[@]}"; do cdargs+=(--cd "$c"); done
    WTL_MEM="$mem" qemu_cmd --name "lab-$LAB-$vm" --disk "$disk" "${cdargs[@]}" \
        --serial "$STATE/$vm.serial.log" --qmp "$sock" "$@" "${opts[@]}"
    start_qemu "$STATE/$vm.qemu.log" || die "qemu ($vm) failed to start"
    echo "$QEMU_PID" >"$STATE/$vm.pid"
    if [[ -n "$vnc" ]]; then echo "VNC 127.0.0.1:$((5900 + vnc))" >"$STATE/$vm.vnc"; else echo "window" >"$STATE/$vm.vnc"; fi
    log "$vm started ($(cat "$STATE/$vm.vnc"))"
}

# uefi_args VM -> echoes "--uefi <varsfile>" if that VM's OS is UEFI, else nothing.
# The vars file is per-VM and lives in STATE so it is preserved as part of the 0 state; it is
# seeded from the OVMF template the first time.
uefi_args() {
    local vm="$1" fw
    fw="$DC_FIRMWARE"; [[ $vm == client ]] && fw="$CL_FIRMWARE"
    [[ "$fw" == uefi ]] || return 0
    local vars="$STATE/$vm-OVMF_VARS.fd"
    [[ -f "$vars" ]] || cp "${WTL_OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}" "$vars"
    printf '%s\n%s\n' --uefi "$vars"
}

# boot_lab DC_DISK CL_DISK DC_CD CL_CD [client qemu args...]
# The client also gets any MOUNT_ISOS as extra CDs (control disc first = D:, mounts after).
boot_lab() {
    local dcd="$1" cld="$2" dccd="$3" clcd="$4"
    shift 4
    port_free "$LAB_LAN_PORT" || die "lab LAN port 127.0.0.1:$LAB_LAN_PORT is in use (another lab running? change LAB_LAN_PORT)"
    rm -f "$STATE"/.off-*
    local dcu=() clu=()
    mapfile -t dcu < <(uefi_args dc)
    mapfile -t clu < <(uefi_args client)
    start_vm dc "$dcd" "$dccd" -- --lan "server:$LAB_LAN_PORT" --lan-mac "$DC_MAC" --no-usernet --no-shutdown "${dcu[@]+"${dcu[@]}"}"
    sleep 1
    start_vm client "$cld" "$clcd" "${MOUNT_ISOS[@]+"${MOUNT_ISOS[@]}"}" -- \
        --lan "client:$LAB_LAN_PORT" --lan-mac "$CL_MAC" --no-shutdown "${clu[@]+"${clu[@]}"}" "$@"
}

LINE=""
# vm_status VM -> the QMP run state ("running", "shutdown", ...); empty if unknown.
vm_status() { qmp "$(vm_sock "$1")" status 2>/dev/null | tr -d '\r\n'; }

# reset_if_off VM -> if the guest powered itself off (status "shutdown"; it stays that way under
# -no-shutdown), issue system_reset so it reboots instead. Some clients power off on a reboot
# request (e.g. Win10 right after a domain join). Returns 0 if it reset, 1 otherwise.
reset_if_off() {
    local vm="$1" st
    st="$(vm_status "$vm")"
    if [[ "$st" == shutdown || "$st" == guest-panicked ]]; then
        warn "$vm powered off on a reboot request; resetting it"
        qmp "$(vm_sock "$vm")" raw '{"execute":"system_reset"}' >/dev/null 2>&1 || true
        return 0
    fi
    return 1
}

# wait_ev VM UNTIL FAIL LABEL : wait for an event on VM's serial log (continues where the last wait
# stopped). If the guest powers off mid-wait (-no-shutdown keeps QEMU alive), reset it and continue.
wait_ev() {
    local vm="$1" rc=0 off=0 r of="$STATE/.off-$1"
    [[ -f "$of" ]] && off="$(cat "$of")"
    while :; do
        # cap each wait at 30s so we can poll the power state (a powered-off guest never emits
        # another serial line and, under -no-shutdown, its pid never dies, so we must check).
        r=$((DEADLINE - SECONDS)); ((r < 1)) && r=1; ((r > 30)) && r=30
        rc=0
        run_bg "$STATE/.match" "$WTL_PYTHON" "$WTL_LIB/serialwait.py" "$STATE/$vm.serial.log" \
            --until "$2" --fail "$3" --timeout "$r" --pid "$(cat "$STATE/$vm.pid")" \
            --from-offset "$off" --offset-file "$of" --progress || rc=$?
        [[ -f "$of" ]] && off="$(cat "$of")"
        if [[ $rc == 3 ]]; then          # this interval saw no event
            reset_if_off "$vm" || true   # reboot it if it powered off; otherwise keep waiting
            ((SECONDS < DEADLINE)) && continue
        fi
        break
    done
    LINE="$(cat "$STATE/.match" 2>/dev/null || true)"
    case $rc in
        0) return 0 ;;
        1) err "$4: $vm reported: $LINE" ;;
        3) err "$4: timeout (--timeout-min $TIMEOUT_MIN)" ;;
        4) err "$4: the $vm VM exited"; sed 's/^/    /' "$STATE/$vm.qemu.log" >&2 || true ;;
        *) err "$4: serialwait error $rc" ;;
    esac
    if [[ -n "$(vm_pid "$vm")" ]]; then
        qmp "$(vm_sock "$vm")" screendump "$STATE/$vm-fail.png" >/dev/null 2>&1 && log "screenshot: $STATE/$vm-fail.png"
    fi
    log "logs: $STATE/$vm.serial.log (guest: C:\\WTL\\agent.log); the VMs keep running - lab restore $LAB"
    [[ $rc == 3 ]] && exit 3
    exit 2
}

# wait_logon VM LABEL : wait for the agent to start (= someone logged on). Windows sometimes leaves
# a domain autologon at the Ctrl+Alt+Del screen, or powers off on a reboot request; reset it (up to
# 3 times) and wait again. wait_ev already resets a powered-off guest, so delegate to it but add an
# extra reset for the "stuck at Ctrl+Alt+Del" case (there the guest is still running, not off).
wait_logon() {
    local vm="$1" try rc off of="$STATE/.off-$1"
    for try in 1 2 3; do
        off=0; [[ -f "$of" ]] && off="$(cat "$of")"
        rc=0
        run_bg "$STATE/.match" "$WTL_PYTHON" "$WTL_LIB/serialwait.py" "$STATE/$vm.serial.log" \
            --until AGENT_START --fail LAB_FAIL --timeout 300 --pid "$(cat "$STATE/$vm.pid")" \
            --from-offset "$off" --offset-file "$of" --progress || rc=$?
        [[ $rc == 0 ]] && return 0
        [[ $rc == 3 ]] || break
        if ! reset_if_off "$vm"; then
            warn "$vm: no logon within 5 min (autologon stuck at the sign-in screen?) - resetting it ($try/3)"
            qmp "$(vm_sock "$vm")" raw '{"execute":"system_reset"}' >/dev/null 2>&1 || true
        fi
    done
    wait_ev "$vm" AGENT_START LAB_FAIL "$2"   # reports the failure and exits
}

# ------------------------------------------------------------------ 0 state
build_bases() {
    local builds=() names=() bpids=() i=0 b bos bname bcn bargs p
    [[ -f "$DC_BASE" ]] || { builds+=("$DC_OS $LAB-dc $LAB_DC_NAME"); names+=(dc); }
    [[ -f "$CL_BASE" ]] || { builds+=("$CL_OS $LAB-client $LAB_CLIENT_NAME"); names+=(client); }
    ((${#builds[@]})) || return 0
    log "installing Windows for: ${names[*]} (unattended; $( ((UPDATES)) && echo 'with Windows Update, hours' || echo 'no updates, ~10 min'))"
    for b in "${builds[@]}"; do
        read -r bos bname bcn <<<"$b"
        bargs=("$bos" --name "$bname" --computer-name "$bcn")
        ((UPDATES)) || bargs+=(--no-updates)
        ((SHOW)) && bargs+=(--show)
        ( "$ROOT/build-image.sh" "${bargs[@]}" 2>&1 | tee "$STATE/build-${names[$i]}.log" | sed -u "s/^/[${names[$i]}] /" ) &
        bpids+=($!)
        i=$((i + 1))
        sleep 10   # the builds pick their VNC displays one after the other
    done
    for p in "${bpids[@]}"; do wait "$p" || true; done
    [[ -f "$DC_BASE" && -f "$CL_BASE" ]] || die "Windows install failed (logs: $STATE/build-*.log)"
}

prepare() {
    [[ -f "$ZERO_DONE" ]] && return 0
    log "=== building the 0 state of lab $LAB (domain $LAB_DOMAIN; once, ~15-25 min) ==="
    restore
    build_bases
    local vm
    for vm in dc client; do
        local base="$DC_BASE"; [[ $vm == client ]] && base="$CL_BASE"
        rm -f "$(zero_disk "$vm")"
        "$WTL_QEMU_IMG" create -f qcow2 -F qcow2 -b "$base" "$(zero_disk "$vm")" >/dev/null \
            || die "qemu-img create $(zero_disk "$vm") failed"
    done
    make_cd dc dc "$STATE/dc-prep.iso" 0
    make_cd client client "$STATE/client-prep.iso" 1
    boot_lab "$(zero_disk dc)" "$(zero_disk client)" "$STATE/dc-prep.iso" "$STATE/client-prep.iso"
    log "creating the domain (DC: AD DS + forest + users, one reboot)"
    wait_ev dc LAB_DC_READY LAB_FAIL,ERROR "domain controller"
    ok "DC ready: $LINE"
    log "joining the client (one reboot, then autologon as $LAB_NETBIOS\\$LAB_USER)"
    wait_ev client LAB_JOINED LAB_FAIL,ERROR "client domain join"
    wait_logon client "client logon after the join"
    wait_ev client LAB_CLIENT_READY LAB_FAIL,ERROR "client"
    ok "client ready: $LINE"
    # the guests report LAB_IDLE (shutdown_when_ready on the client); clean ACPI power-off, client first
    wait_ev client LAB_IDLE LAB_FAIL,ERROR "client"
    sleep 20   # let the first logon finish (profile, group policy) before it is frozen
    local v
    for v in client dc; do
        log "shutting the $v down cleanly"
        freeze_off "$v" || die "$v did not power off; 0 state not saved (lab rebuild $LAB, then retry)"
        rm -f "$STATE/$v.pid"
    done
    rm -f "$STATE/dc-prep.iso" "$STATE/client-prep.iso"
    py -c 'import json,sys,time; json.dump({"lab":sys.argv[2],"built_at":time.strftime("%Y-%m-%d %H:%M")},open(sys.argv[1],"w"))' "$ZERO_DONE" "$LAB"
    ok "0 state saved: $(zero_disk dc), $(zero_disk client)"
}

prepare
[[ $CMD == prepare ]] && exit 0

# ------------------------------------------------------------------ run / up: boot from the 0 state
RES=""
if [[ $CMD == run ]]; then
    RES="$STATE/runs/$RUN_ID"
    mkdir -p "$RES"
    ln -sfn "runs/$RUN_ID" "$STATE/last"
fi
restore
for vm in dc client; do
    "$WTL_QEMU_IMG" create -f qcow2 -F qcow2 -b "$(zero_disk "$vm")" "$(run_disk "$vm")" >/dev/null \
        || die "qemu-img create $(run_disk "$vm") failed"
done
make_cd dc dc "$STATE/dc.iso"
make_cd client client "$STATE/client.iso"
rm -f "$STATE"/mount-*.iso
[[ -n "$CONFIG" ]] && make_mount_isos
log "lab $LAB: booting from the 0 state"
boot_lab "$(run_disk dc)" "$(run_disk client)" "$STATE/dc.iso" "$STATE/client.iso"
wait_ev dc LAB_DC_READY LAB_FAIL,ERROR "domain controller"
ok "DC ready"
wait_logon client "client logon"
wait_ev client LAB_CLIENT_READY LAB_FAIL,ERROR "client"
ok "client ready: $LAB_NETBIOS\\$LAB_USER logged on"

summary() {
    cat >&2 <<EOF

  lab $LAB: domain $LAB_DOMAIN
    DC      $LAB_DC_NAME  $DC_IP  $(cat "$STATE/dc.vnc")   $LAB_NETBIOS\\Administrator / $WTL_PASSWORD
    client  $LAB_CLIENT_NAME  $CL_IP  $(cat "$STATE/client.vnc")   logged on: $LAB_NETBIOS\\$LAB_USER / $LAB_USER_PASSWORD
  back to the 0 state: lab restore $LAB      another run: lab run CONFIG (restores by itself)
EOF
}
if [[ $CMD == up ]]; then summary; exit 0; fi

# ---- config run: mounts + the steps (run_as_admin / run_as_student), in the order written
RES="$STATE/runs/$RUN_ID"
log "running config $(basename "$CONFIG") (mounts + steps)"
STATUS=0
# loop over events until LAB_RUN_DONE: LAB_STEP reports each script as it finishes
while :; do
    wait_ev client "LAB_RUN_DONE,LAB_STEP" LAB_FAIL,ERROR "config run"
    case "$LINE" in
        "WTL LAB_STEP"*)
            as="$(sed -n 's/.* as=\([^ ]*\).*/\1/p' <<<"$LINE")"
            sc="$(sed -n 's/.* script=\([^ ]*\).*/\1/p' <<<"$LINE" | py -c 'import sys,urllib.parse;print(urllib.parse.unquote(sys.stdin.read().strip()))')"
            ex="$(sed -n 's/.* exit=\([^ ]*\).*/\1/p' <<<"$LINE")"
            if [[ "$ex" == 0 ]]; then ok "[$as] $sc -> exit 0"
            else err "[$as] $sc -> exit $ex"; STATUS=1; fi
            ;;
        "WTL LAB_RUN_DONE"*)
            ok "all steps done"
            break ;;
    esac
done
# decode the per-step stdout/stderr the guest sent (stepNout / stepNerr streams)
mkdir -p "$RES"
py "$WTL_LIB/outputs.py" "$STATE/client.serial.log" "$RES" >/dev/null 2>&1 || true
sleep 2
qmp "$(vm_sock client)" screendump "$RES/screen.png" >/dev/null 2>&1 || true
cp "$STATE/client.serial.log" "$RES/client.serial.log" 2>/dev/null || true
cp "$CONFIG" "$RES/config.conf" 2>/dev/null || true
summary
echo "  config: $(basename "$CONFIG")  ->  $( ((STATUS == 0)) && echo 'ALL STEPS OK' || echo 'A STEP FAILED' )" >&2
echo "  results: $RES  (step output: $RES/step*.txt, screen.png = the desktop)" >&2
exit "$STATUS"
