#!/usr/bin/env bash
# launch-test.sh - boot a throwaway copy of a base image, run a program from a zip, test it.
#
#   ./launch-test.sh ZIP EXE OS [options]
#     ZIP              zip on the host
#     EXE              file name or relative path inside the zip (program.exe, bin/program.exe)
#     OS               2012 (2012r2) 2016 2019 2022 2025, or "all" (every built image)
#   options:
#     --test FILE      host-side test (python3 FILE; *.sh run with bash). Optional.
#     --mode M         "exit"   (default without --port): run EXE until it exits, capture
#                               stdout/stderr + exit code into the results dir
#                      "server" (default with --port): EXE keeps running, the test talks
#                               to it over the forwarded port(s)
#     --port P[:H]     forward guest TCP port P to 127.0.0.1:H (default H=P). Repeatable.
#     --args "..."     command-line args for EXE
#     --wait-port P    guest port that must listen before READY (default: first --port)
#     --expect-exit N  exit mode: required exit code (default 0 when no --test is given;
#                      with --test the script decides unless --expect-exit is also given)
#     --run-timeout S  exit mode: kill EXE if still running after S seconds (default 300)
#     --timeout S      overall seconds incl. boot before failure (default 900)
#     --show           show the VM window (gtk); default headless + VNC on 127.0.0.1:59xx
#     --keep           keep the VM running afterwards (and its overlay)
#     --test-arg A     extra argument for the test script (repeatable)
# Exit: 0 pass, 1 test failed, 2 setup/launch error, 3 timeout. For "all": the worst code.
set -euo pipefail
# shellcheck source=lib/common.sh
source "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/lib/common.sh"

usage() {
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-2}"
}

POS=() TEST="" PORTS=() EXE_ARGS="" WAIT_PORT="" TIMEOUT=900 SHOW=0 KEEP=0 TEST_ARGS=()
MODE="" EXPECT_EXIT="" RUN_TIMEOUT=300
while (($#)); do
    case "$1" in
        --test) TEST="${2:?--test needs a file}"; shift 2 ;;
        --port) PORTS+=("${2:?--port needs P[:H]}"); shift 2 ;;
        --args) EXE_ARGS="${2-}"; shift 2 ;;
        --wait-port) WAIT_PORT="${2:?}"; shift 2 ;;
        --mode) MODE="${2:?--mode needs exit|server}"; shift 2 ;;
        --expect-exit) EXPECT_EXIT="${2:?}"; shift 2 ;;
        --run-timeout) RUN_TIMEOUT="${2:?}"; shift 2 ;;
        --timeout) TIMEOUT="${2:?}"; shift 2 ;;
        --show) SHOW=1; shift ;;
        --keep) KEEP=1; shift ;;
        --test-arg) TEST_ARGS+=("${2-}"); shift 2 ;;
        -h|--help) usage 0 ;;
        --) shift; POS+=("$@"); break ;;
        -*) err "unknown option $1"; usage 2 ;;
        *) POS+=("$1"); shift ;;
    esac
done
((${#POS[@]} == 3)) || { err "need ZIP EXE OS"; usage 2; }
ZIP="${POS[0]}" EXE="${POS[1]}" OS_ARG="${POS[2]}"

[[ "$TIMEOUT" =~ ^[0-9]+$ && "$TIMEOUT" -gt 0 ]] || die "--timeout must be a positive integer"
[[ "$RUN_TIMEOUT" =~ ^[0-9]+$ && "$RUN_TIMEOUT" -gt 0 ]] || die "--run-timeout must be a positive integer"
[[ -z "$EXPECT_EXIT" || "$EXPECT_EXIT" =~ ^-?[0-9]+$ ]] || die "--expect-exit must be an integer"
if [[ -z "$MODE" ]]; then
    if ((${#PORTS[@]})) || [[ -n "$WAIT_PORT" ]]; then MODE=server; else MODE=exit; fi
fi
[[ "$MODE" == exit || "$MODE" == server ]] || die "--mode must be exit or server"
if [[ "$MODE" == exit && -z "$TEST" && -z "$EXPECT_EXIT" ]]; then EXPECT_EXIT=0; fi
if [[ "$MODE" == exit ]] && ((RUN_TIMEOUT + 60 > TIMEOUT)); then
    TIMEOUT=$((RUN_TIMEOUT + 600))
    log "--timeout raised to $TIMEOUT s (boot + --run-timeout $RUN_TIMEOUT)"
fi
[[ -f "$ZIP" ]] || die "zip not found: $ZIP"
ZIP="$(readlink -f "$ZIP")"
if [[ -n "$TEST" ]]; then
    [[ -f "$TEST" ]] || die "test script not found: $TEST"
    TEST="$(readlink -f "$TEST")"
fi

# ---- ports: GUEST[:HOST]
GPORTS=() HPORTS=()
for p in "${PORTS[@]+"${PORTS[@]}"}"; do
    if [[ "$p" =~ ^([0-9]+)(:([0-9]+))?$ ]]; then
        g="${BASH_REMATCH[1]}" h="${BASH_REMATCH[3]:-${BASH_REMATCH[1]}}"
        ((g >= 1 && g <= 65535 && h >= 1 && h <= 65535)) || die "bad port $p"
        GPORTS+=("$g") HPORTS+=("$h")
    else
        die "bad --port '$p' (want P or P:H)"
    fi
done
if [[ -n "$WAIT_PORT" ]]; then
    [[ "$WAIT_PORT" =~ ^[0-9]+$ ]] || die "bad --wait-port"
elif ((${#GPORTS[@]})) && [[ "$MODE" == server ]]; then
    WAIT_PORT="${GPORTS[0]}"
fi

# ---- zip / exe validation (python zipfile, case-insensitive)
ZIP_MEMBER="$(py - "$ZIP" "$EXE" <<'EOF'
import sys, zipfile
z, exe = sys.argv[1], sys.argv[2]
try:
    zf = zipfile.ZipFile(z)
    bad = zf.testzip()
except (zipfile.BadZipFile, OSError) as ex:
    sys.stderr.write("not a valid zip: %s (%s)\n" % (z, ex)); sys.exit(2)
if bad:
    sys.stderr.write("corrupt member in zip: %s\n" % bad); sys.exit(2)
names = [n for n in zf.namelist() if not n.endswith("/")]
want = exe.replace("\\", "/").lstrip("/")
while want.startswith("./"):
    want = want[2:]
wl = want.lower()
hit = [n for n in names if n.lower() == wl]
if not hit and "/" in wl:
    hit = [n for n in names if n.lower().endswith("/" + wl)]
if not hit:
    base = wl.rsplit("/", 1)[-1]
    hit = [n for n in names if n.lower().rsplit("/", 1)[-1] == base]
if not hit:
    sys.stderr.write("'%s' not found in %s; it contains:\n" % (exe, z))
    for n in names[:30]:
        sys.stderr.write("    %s\n" % n)
    sys.exit(1)
print(hit[0])
EOF
)" || die "invalid ZIP/EXE"
log "zip ok: $(basename "$ZIP") contains $ZIP_MEMBER"

# ---- OS list
KEYS=()
if [[ "$OS_ARG" == all ]]; then
    while IFS= read -r k; do
        [[ -f "$WTL_IMAGES_DIR/$k.qcow2" ]] && KEYS+=("$k")
    done < <(os_keys)
    ((${#KEYS[@]})) || die "all: no base images in $WTL_IMAGES_DIR (run ./build-image.sh first)"
else
    os_lookup "$OS_ARG" || die "unknown OS '$OS_ARG' (known: $(os_keys | tr '\n' ' ') all)"
    KEYS=("$OS_KEY")
    [[ -f "$WTL_IMAGES_DIR/$OS_KEY.qcow2" ]] \
        || die "base image $WTL_IMAGES_DIR/$OS_KEY.qcow2 not found (run ./build-image.sh $OS_KEY)"
fi
if ((KEEP && ${#KEYS[@]} > 1 && ${#HPORTS[@]} > 0)); then
    die "--keep with several OSes would leave VMs fighting over the same host ports; run them one at a time"
fi

check_tools "$WTL_QEMU" "$WTL_QEMU_IMG"
mkdir -p "$WTL_RUNS_DIR" "$WTL_RESULTS_DIR"

# ------------------------------------------------------------------ per-run state + trap
CUR_PID="" CUR_SOCK="" CUR_RUN_DIR="" CUR_KEEP=0
cleanup() {
    if [[ -n "$CUR_PID" && $CUR_KEEP -eq 0 ]]; then
        stop_vm "$CUR_PID" "$CUR_SOCK" 10
        [[ -n "$CUR_RUN_DIR" ]] && rm -rf "$CUR_RUN_DIR"
    fi
    CUR_PID=""
}
on_signal() {
    trap - INT TERM
    echo >&2
    warn "interrupted - stopping VM"
    kill_bg
    CUR_KEEP=0
    cleanup
    exit 2
}
trap on_signal INT TERM
trap cleanup EXIT

now() { date +%s.%N; }

write_result() {  # RESULTS_DIR OS RUN_ID STATUS TEST_EXIT T_START T_AGENT T_LAUNCH T_READY T_TEST_END SERIAL EXTRA_JSON
    py - "$@" <<'EOF'
import json, re, sys, time
(res, os_, run_id, status, test_exit, t0, ta, tl, tr, tt, serial, extra) = sys.argv[1:13]
def f(x):
    try: return float(x)
    except ValueError: return None
t0, ta, tl, tr, tt = map(f, (t0, ta, tl, tr, tt))
end = time.time()
def d(a, b):
    return round(b - a, 2) if a is not None and b is not None else None
events = []
try:
    for line in open(serial, "rb").read().decode("utf-8", "replace").splitlines():
        line = line.replace("\r", "").replace("\0", "")
        m = re.search(r"WTL [A-Z][A-Z0-9_]*.*$", line)
        if m and not m.group(0).startswith("WTL OUTPUT "):   # output chunks -> stdout.txt/stderr.txt
            events.append(m.group(0).strip())
except OSError:
    pass
out = {"os": os_, "run_id": run_id, "status": status,
       "test_exit": int(test_exit) if test_exit.lstrip("-").isdigit() else None,
       "durations": {"boot": d(t0, ta), "launch": d(ta, tl), "ready": d(tl, tr),
                     "test": d(tr, tt), "total": d(t0, end)},
       "events": events}
out.update(json.loads(extra or "{}"))
json.dump(out, open(res + "/result.json", "w"), indent=1)
EOF
}

# ------------------------------------------------------------------ one run
run_one() {
    local key="$1"
    os_lookup "$key"
    local stamp run_id run_dir res_dir overlay job sock serial vnc="" i
    stamp="$(date +%Y%m%d-%H%M%S)"
    run_id="$stamp-$key"
    i=1
    while [[ -e "$WTL_RESULTS_DIR/$run_id" || -e "$WTL_RUNS_DIR/$run_id" ]]; do
        run_id="$stamp-$key-$i"; i=$((i + 1))
    done
    run_dir="$WTL_RUNS_DIR/$run_id"
    res_dir="$WTL_RESULTS_DIR/$run_id"
    mkdir -p "$run_dir/job" "$res_dir"
    CUR_RUN_DIR="$run_dir" CUR_KEEP=0
    log "=== $OS_DISPLAY: run $run_id ==="

    local T0 TA="" TL="" TR="" TT="" status="error" code=2 test_exit="" sw=0 msg=""
    T0="$(now)"
    local deadline=$((SECONDS + TIMEOUT))
    remaining() { local r=$((deadline - SECONDS)); ((r < 1)) && r=1; echo "$r"; }

    _fail() {  # status code message
        status="$1" code="$2" msg="$3"
        err "$msg"
    }

    # ---- host ports free?
    local j fwd=()
    for j in "${!HPORTS[@]}"; do
        if ! port_free "${HPORTS[$j]}"; then
            _fail error 2 "host port 127.0.0.1:${HPORTS[$j]} is already in use"
            write_result "$res_dir" "$key" "$run_id" "$status" "" "$T0" "" "" "" "" /dev/null \
                "{\"error\": \"host port ${HPORTS[$j]} in use\"}"
            rm -rf "$run_dir"; CUR_RUN_DIR=""
            return 2
        fi
        fwd+=(--fwd "${HPORTS[$j]}:${GPORTS[$j]}")
    done

    # ---- overlay + job iso
    overlay="$run_dir/disk.qcow2"
    if ! "$WTL_QEMU_IMG" create -f qcow2 -F qcow2 -b "$WTL_IMAGES_DIR/$key.qcow2" "$overlay" >"$run_dir/qemu-img.log" 2>&1; then
        sed 's/^/    /' "$run_dir/qemu-img.log" >&2
        _fail error 2 "qemu-img create overlay failed"
        write_result "$res_dir" "$key" "$run_id" "$status" "" "$T0" "" "" "" "" /dev/null '{"error":"overlay"}'
        rm -rf "$run_dir"; CUR_RUN_DIR=""
        return 2
    fi
    job="$run_dir/job"
    local job_to="$TIMEOUT"
    [[ "$MODE" == exit ]] && job_to="$RUN_TIMEOUT"
    py - "$job/JOB.JSON" "$run_id" "$(basename "$ZIP")" "$EXE" "$EXE_ARGS" "$WAIT_PORT" "$job_to" "$MODE" <<'EOF'
import json, sys
p, run_id, zip_name, exe, args, wp, to, mode = sys.argv[1:9]
json.dump({"run_id": run_id, "zip_name": zip_name, "exe": exe, "args": args,
           "wait_ports": [int(wp)] if wp else [], "timeout": int(to), "mode": mode},
          open(p, "w"), indent=1)
EOF
    cp "$ZIP" "$job/PAYLOAD.ZIP"
    if ! py "$WTL_LIB/mkiso.py" -o "$run_dir/job.iso" -V WTL_JOB "$job" 2>"$run_dir/mkiso.log"; then
        sed 's/^/    /' "$run_dir/mkiso.log" >&2
        _fail error 2 "mkiso failed"
        write_result "$res_dir" "$key" "$run_id" "$status" "" "$T0" "" "" "" "" /dev/null '{"error":"mkiso"}'
        rm -rf "$run_dir"; CUR_RUN_DIR=""
        return 2
    fi
    rm -rf "$job"

    # ---- qemu
    sock="$(sock_path "$run_dir/qmp.sock" "$run_id")"
    serial="$run_dir/serial.log"
    : >"$serial"
    local opts=()
    if ((SHOW)); then opts+=(--show); else vnc="$(pick_vnc_display)"; opts+=(--vnc "$vnc"); fi
    qemu_cmd --name "wtl-$run_id" --disk "$overlay" --cd "$run_dir/job.iso" \
        --serial "$serial" --qmp "$sock" "${fwd[@]+"${fwd[@]}"}" "${opts[@]}"
    if ! start_qemu "$run_dir/qemu.log"; then
        cp "$run_dir/qemu.log" "$res_dir/" 2>/dev/null || true
        _fail error 2 "qemu failed to start"
        write_result "$res_dir" "$key" "$run_id" "$status" "" "$T0" "" "" "" "" /dev/null '{"error":"qemu start"}'
        rm -rf "$run_dir"; CUR_RUN_DIR=""
        return 2
    fi
    CUR_PID="$QEMU_PID" CUR_SOCK="$sock"
    echo "$QEMU_PID" >"$run_dir/qemu.pid"
    log "qemu pid $QEMU_PID$([[ -n "$vnc" ]] && echo "; VNC: vncviewer 127.0.0.1:$((5900 + vnc))")"
    for j in "${!HPORTS[@]}"; do log "port: guest ${GPORTS[$j]} -> 127.0.0.1:${HPORTS[$j]}"; done

    # ---- wait for events in stages (each stage gets the remaining overall budget)
    local offf="$run_dir/.offset" off=0 line
    echo 0 >"$offf"
    stage() {  # UNTIL FAIL LABEL
        off="$(cat "$offf")"
        sw=0
        run_bg "$run_dir/.match" "$WTL_PYTHON" "$WTL_LIB/serialwait.py" "$serial" --until "$1" --fail "$2" \
            --timeout "$(remaining)" --pid "$QEMU_PID" --from-offset "$off" \
            --offset-file "$offf" --progress || sw=$?
        line="$(cat "$run_dir/.match" 2>/dev/null || true)"
        case $sw in
            0) return 0 ;;
            1) _fail error 2 "$3: guest reported: $line" ;;
            3) _fail timeout 3 "$3: timeout (overall --timeout $TIMEOUT s)" ;;
            4) _fail error 2 "$3: qemu exited unexpectedly"
               sed 's/^/    /' "$run_dir/qemu.log" >&2 || true ;;
            *) _fail error 2 "$3: serialwait error $sw" ;;
        esac
        return 1
    }
    local okrun=0 exitinfo="{}" exit_code="" exit_ok=1
    log "waiting for the guest agent (boot)..."
    if stage AGENT_START LAUNCH_FAIL "boot"; then
        TA="$(now)"
        if stage LAUNCHED LAUNCH_FAIL,ERROR,PROC_EXIT "launch"; then
            TL="$(now)"
            if [[ "$MODE" == exit ]]; then
                log "program started; waiting for it to finish (max $RUN_TIMEOUT s)..."
                if stage PROC_EXIT,PROC_TIMEOUT LAUNCH_FAIL,ERROR "run"; then
                    TR="$(now)"
                    exitinfo="$(py "$WTL_LIB/outputs.py" "$serial" "$res_dir")" || exitinfo="{}"
                    exit_code="$(py -c 'import json,sys;v=json.loads(sys.argv[1]).get("exit_code");print("" if v is None else v)' "$exitinfo")"
                    local ename timed
                    ename="$(py -c 'import json,sys;d=json.loads(sys.argv[1]);print(d.get("exit_hex") or "", d.get("exit_name") or "")' "$exitinfo")"
                    timed="$(py -c 'import json,sys;print(int(bool(json.loads(sys.argv[1]).get("timed_out"))))' "$exitinfo")"
                    echo "----- stdout (first 40 lines; full: $res_dir/stdout.txt) -----" >&2
                    head -n 40 "$res_dir/stdout.txt" >&2 2>/dev/null || true
                    if [[ -s "$res_dir/stderr.txt" ]]; then
                        echo "----- stderr (first 20 lines) -----" >&2
                        head -n 20 "$res_dir/stderr.txt" >&2
                    fi
                    echo "-----" >&2
                    if ((timed)); then
                        _fail timeout 3 "program did not exit within --run-timeout $RUN_TIMEOUT s (killed)"
                        exit_ok=0
                    else
                        log "program exited with code $exit_code ($ename)"
                        if [[ -n "$EXPECT_EXIT" && "$exit_code" != "$EXPECT_EXIT" ]]; then
                            exit_ok=0
                        fi
                        okrun=1
                    fi
                fi
            elif stage READY LAUNCH_FAIL,ERROR,PROC_EXIT "ready"; then
                TR="$(now)"
                okrun=1
                ok "READY: $line"
            fi
        fi
    fi

    # ---- test script
    if ((okrun)); then
        if [[ -n "$TEST" ]]; then
            log "running test: $(basename "$TEST")"
            local runner=(python3) tcmd envs=()
            [[ "$TEST" == *.sh ]] && runner=(bash)
            envs=(WTL_HOST=127.0.0.1 "WTL_OS=$key" "WTL_RESULTS=$res_dir" "WTL_RUN_ID=$run_id" "WTL_MODE=$MODE")
            if [[ "$MODE" == exit ]]; then
                envs+=("WTL_EXIT_CODE=$exit_code" "WTL_STDOUT=$res_dir/stdout.txt" "WTL_STDERR=$res_dir/stderr.txt")
            fi
            ((${#HPORTS[@]})) && envs+=("WTL_PORT=${HPORTS[0]}")
            for j in "${!HPORTS[@]}"; do envs+=("WTL_PORT_${GPORTS[$j]}=${HPORTS[$j]}"); done
            tcmd=(env "${envs[@]}")
            if command -v timeout >/dev/null 2>&1; then tcmd+=(timeout -k 5 "$(remaining)"); fi
            tcmd+=("${runner[@]}" "$TEST" "${TEST_ARGS[@]+"${TEST_ARGS[@]}"}")
            set +e
            "${tcmd[@]}" 2>&1 | tee "$res_dir/test.log"
            test_exit=${PIPESTATUS[0]}
            set -e
            TT="$(now)"
            if ((test_exit == 0 && exit_ok)); then
                status=pass code=0
                ok "test passed"
            elif ((test_exit == 0)); then
                _fail fail 1 "test passed, but program exit code $exit_code != --expect-exit $EXPECT_EXIT"
            elif ((test_exit == 124 || test_exit == 137)) && ((SECONDS >= deadline - 1)); then
                _fail timeout 3 "test script timed out"
            else
                _fail fail 1 "test failed (exit $test_exit)"
            fi
        elif [[ "$MODE" == exit ]]; then
            if ((exit_ok)); then
                status=pass code=0
                ok "program exited with $exit_code as expected"
            else
                _fail fail 1 "program exit code $exit_code, expected $EXPECT_EXIT"
            fi
        else
            status=pass code=0
            ok "program is up (no --test given)"
        fi
    fi

    # ---- collect
    if pid_alive "$QEMU_PID" && [[ -S "$sock" ]]; then
        qmp "$sock" screendump "$res_dir/screen.png" >/dev/null 2>&1 || warn "screendump failed"
    fi
    cp "$serial" "$res_dir/serial.log" 2>/dev/null || true
    cp "$run_dir/qemu.log" "$res_dir/qemu.log" 2>/dev/null || true
    local extra
    extra="$(py -c 'import json,sys; a=sys.argv; print(json.dumps({"zip":a[1],"exe":a[2],"args":a[3],"ports":{g:int(h) for g,h in zip(a[4].split(),a[5].split())},"vnc":(a[6] or None),"test":(a[7] or None),"message":a[8],"kept":a[9]=="1","overlay":(a[10] if a[9]=="1" else None),"qmp":(a[11] if a[9]=="1" else None)}))' \
        "$ZIP" "$EXE" "$EXE_ARGS" "${GPORTS[*]-}" "${HPORTS[*]-}" \
        "$([[ -n "$vnc" ]] && echo "127.0.0.1:$((5900 + vnc))")" "$TEST" "$msg" "$KEEP" "$overlay" "$sock")"
    extra="$(py -c 'import json,sys;a=json.loads(sys.argv[1]);a["mode"]=sys.argv[2];a["program"]=json.loads(sys.argv[3]);a["expect_exit"]=(int(sys.argv[4]) if sys.argv[4] else None);print(json.dumps(a))' \
        "$extra" "$MODE" "$exitinfo" "$EXPECT_EXIT")"
    write_result "$res_dir" "$key" "$run_id" "$status" "$test_exit" "$T0" "$TA" "$TL" "$TR" "$TT" "$serial" "$extra"

    if ((KEEP)) && pid_alive "$QEMU_PID"; then
        CUR_KEEP=1
        ok "VM kept running (pid $QEMU_PID)"
        [[ -n "$vnc" ]] && echo "    connect:  vncviewer 127.0.0.1:$((5900 + vnc))" >&2
        echo "    overlay:  $overlay" >&2
        echo "    serial:   $serial" >&2
        echo "    stop it:  python3 $WTL_LIB/qmp.py '$sock' quit   (then rm -rf '$run_dir')" >&2
    else
        stop_vm "$QEMU_PID" "$sock" 10
        rm -rf "$run_dir"
    fi
    CUR_PID="" CUR_RUN_DIR=""
    log "results: $res_dir  (status: $status)"
    LAST_RES_DIR="$res_dir" LAST_STATUS="$status"
    return "$code"
}

WORST=0
declare -A RC=() ST=() RD=()
for k in "${KEYS[@]}"; do
    rc=0 LAST_RES_DIR="" LAST_STATUS="error"
    run_one "$k" || rc=$?
    RC[$k]=$rc ST[$k]="$LAST_STATUS" RD[$k]="$LAST_RES_DIR"
    ((rc > WORST)) && WORST=$rc
done

if ((${#KEYS[@]} > 1)); then
    printf '\n%-6s %-8s %-4s %s\n' OS STATUS EXIT RESULTS
    for k in "${KEYS[@]}"; do
        printf '%-6s %-8s %-4s %s\n' "$k" "${ST[$k]}" "${RC[$k]}" "${RD[$k]:--}"
    done
fi
exit "$WORST"
