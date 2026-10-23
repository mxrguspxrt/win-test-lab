#!/usr/bin/env bash
# tests/run-tests.sh - host-logic tests for win-test-lab without KVM / Windows.
#
# Runs the real build-image.sh / launch-test.sh against tests/fake-qemu and
# tests/fake-qemu-img. The project is copied into a temp dir first, so runs/,
# results/ and images/ of your real checkout are never touched.
#
# Usage: tests/run-tests.sh [-k|--keep-tmp] [-v|--verbose] [SCENARIO_LETTER...]
#   e.g. tests/run-tests.sh c d     # only scenarios c and d (a/b still provide images)
# Env:  FAKE_QEMU_SPEED (default 1)   SCEN_TIMEOUT (default 120 s per scenario)
set -uo pipefail

TESTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TESTS/.." && pwd)"
KEEP_TMP=0; VERBOSE=0; ONLY=()
for a in "$@"; do
  case "$a" in
    -k|--keep-tmp) KEEP_TMP=1 ;;
    -v|--verbose) VERBOSE=1 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) ONLY+=("$a") ;;
  esac
done
SCEN_TIMEOUT="${SCEN_TIMEOUT:-120}"

# ---------------------------------------------------------------- prerequisites
skip_all() { echo "run-tests: SKIPPED - $*"; exit 0; }
command -v python3 >/dev/null || skip_all "python3 not found"
command -v timeout >/dev/null || skip_all "coreutils 'timeout' not found"
[ -f "$ROOT/lib/mkiso.py" ] || skip_all "lib/mkiso.py does not exist yet (needed to build fake install ISOs)"
[ -f "$ROOT/build-image.sh" ] || [ -f "$ROOT/launch-test.sh" ] || \
  skip_all "neither build-image.sh nor launch-test.sh exists yet"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/wtl-tests.XXXXXX")"
LAB="$TMP/lab"            # copy of the project
PIDDIR="$TMP/qemu-pids"   # fake-qemu registers live pids here
mkdir -p "$LAB" "$PIDDIR" "$TMP/isos" "$TMP/images" "$TMP/logs" "$TMP/data"

cleanup() {
  local p leaked=0
  for f in "$PIDDIR"/*; do
    [ -e "$f" ] || continue
    p="$(basename "$f")"
    if kill -0 "$p" 2>/dev/null; then kill "$p" 2>/dev/null; leaked=1; fi
  done
  [ "$leaked" = 1 ] && sleep 1
  for f in "$PIDDIR"/*; do
    [ -e "$f" ] || continue
    kill -9 "$(basename "$f")" 2>/dev/null
  done
  if [ "$KEEP_TMP" = 1 ]; then echo "run-tests: temp dir kept: $TMP"; else rm -rf "$TMP"; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# copy project (without heavy / stateful dirs; iso-files holds multi-GB installer ISOs)
( cd "$ROOT" && for e in * .[!.]*; do
    case "$e" in images|iso-files|runs|results|tests|wtl-rs|.git|'*'|'.[!.]*') continue ;; esac
    cp -a "$e" "$LAB/" 2>/dev/null
  done )
mkdir -p "$LAB/tests"
cp -a "$TESTS/fake-qemu" "$TESTS/fake-qemu-img" "$LAB/tests/"
chmod +x "$LAB/tests/fake-qemu" "$LAB/tests/fake-qemu-img" "$LAB"/*.sh 2>/dev/null

export WTL_QEMU="$LAB/tests/fake-qemu"
export WTL_QEMU_IMG="$LAB/tests/fake-qemu-img"
export WTL_NO_KVM=1
export WTL_IMAGES_DIR="$TMP/images"
export WTL_ISO_DIR="$TMP/isos"
export WTL_MEM=512
export FAKE_QEMU_LOG="$TMP/logs/fake-qemu-argv.log"
export FAKE_QEMU_PIDDIR="$PIDDIR"
export FAKE_QEMU_MAX_LIFETIME=300
export FAKE_QEMU_SPEED="${FAKE_QEMU_SPEED:-1}"
: > "$FAKE_QEMU_LOG"

# ---------------------------------------------------------------- fixtures
# fake install media: sources/install.wim = MSWIM header + UTF-16LE XML
make_install_iso() { # build iso_name [client]
  local build="$1" name="$2" kind="${3:-server}" src="$TMP/data/inst-$1"
  mkdir -p "$src/sources" "$src/boot"
  python3 - "$build" "$src/sources/install.wim" "$kind" <<'PY'
import struct, sys
build, out, kind = int(sys.argv[1]), sys.argv[2], sys.argv[3]
imgs = []
editions = [("ServerStandard", "Server Core", "SERVERSTANDARDCORE"),
            ("ServerStandard", "Server", "SERVERSTANDARD"),
            ("ServerDatacenter", "Server Core", "SERVERDATACENTERCORE"),
            ("ServerDatacenter", "Server", "SERVERDATACENTER")]
if kind == "client":
    editions = [("Core", "Client", "CORE"), ("Education", "Client", "EDUCATION"),
                ("Enterprise", "Client", "ENTERPRISE"), ("Professional", "Client", "PROFESSIONAL")]
for i, (ed, it, nm) in enumerate(editions, 1):
    imgs.append('<IMAGE INDEX="%d"><NAME>Windows Server %s</NAME><WINDOWS><ARCH>9</ARCH>'
                '<EDITIONID>%s</EDITIONID><INSTALLATIONTYPE>%s</INSTALLATIONTYPE>'
                '<VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>%d</BUILD><SPBUILD>0</SPBUILD></VERSION>'
                '</WINDOWS></IMAGE>' % (i, nm, ed, it, build))
xml = ("<WIM><TOTALBYTES>1234</TOTALBYTES>%s</WIM>" % "".join(imgs)).encode("utf-16-le")
xml = b"\xff\xfe" + xml
hdr = bytearray(208)
hdr[0:8] = b"MSWIM\0\0\0"
struct.pack_into("<I", hdr, 8, 208)           # header size
struct.pack_into("<I", hdr, 12, 0x10d00)      # version
struct.pack_into("<I", hdr, 20, 32768)        # chunk size
struct.pack_into("<H", hdr, 40, 1)            # part number
struct.pack_into("<H", hdr, 42, 1)            # total parts
struct.pack_into("<I", hdr, 44, len(imgs))    # image count
off = 208 + 4096
struct.pack_into("<Q", hdr, 0x48, len(xml) | (0x02 << 56))   # xml reshdr: size | flags(metadata)
struct.pack_into("<Q", hdr, 0x50, off)
struct.pack_into("<Q", hdr, 0x58, len(xml))
with open(out, "wb") as f:
    f.write(bytes(hdr) + b"\0" * 4096 + xml)
PY
  echo "fake boot" > "$src/boot/bootfix.bin"
  python3 "$LAB/lib/mkiso.py" -o "$TMP/isos/$name" -V "FAKE_$build" "$src" >/dev/null
}
make_install_iso 9600  "9600.17050.WINBLUE_REFRESH.140317-1640_X64FRE_SERVER_EVAL_EN-US-IR3_SSS_X64FREE_EN-US_DV9.ISO" \
  || skip_all "mkiso.py failed to build the fake install ISO"
make_install_iso 14393 "14393.0.161119-1705.RS1_REFRESH_SERVER_EVAL_X64FRE_EN-US.ISO" \
  || skip_all "mkiso.py failed to build the fake install ISO"
make_install_iso 26300 "win11_24h2_en_x64_26300.iso" client \
  || skip_all "mkiso.py failed to build the fake install ISO"
# UEFI firmware stand-ins (fake-qemu only checks they are passed as pflash)
echo "fake OVMF code" > "$TMP/data/OVMF_CODE.fd"
echo "fake OVMF vars" > "$TMP/data/OVMF_VARS.fd"
export WTL_OVMF_CODE="$TMP/data/OVMF_CODE.fd" WTL_OVMF_VARS="$TMP/data/OVMF_VARS.fd"

# demo zips
DEMO_ZIP="$TMP/data/program.zip"
bash "$ROOT/examples/make-demo-zip.sh" "$DEMO_ZIP" >/dev/null || { echo "make-demo-zip.sh failed"; exit 1; }
behavior_zip() { # mode-json out
  local d="$TMP/data/beh-$RANDOM"; mkdir -p "$d"
  printf '%s\n' "$1" > "$d/fake-behavior.json"
  bash "$ROOT/examples/make-demo-zip.sh" "$2" "$d/fake-behavior.json" >/dev/null
}
behavior_zip '{"mode":"crash","code":3}' "$TMP/data/crash.zip"
behavior_zip '{"mode":"wrong"}'          "$TMP/data/wrong.zip"
behavior_zip '{"mode":"hang"}'           "$TMP/data/hang.zip"
PYTEST="$ROOT/examples/python-test.py"

free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }

# ---------------------------------------------------------------- harness
NAMES=(); RESULTS=(); NOTES=(); TIMES=()
CUR_LOG=""; CUR_RC=0; CUR_SECS=0

want() { # letter
  [ "${#ONLY[@]}" -eq 0 ] && return 0
  local x; for x in "${ONLY[@]}"; do [ "$x" = "$1" ] && return 0; done; return 1
}
record() { NAMES+=("$1"); RESULTS+=("$2"); NOTES+=("$3"); TIMES+=("$4");
  printf '  -> %s %s\n' "$2" "$3"; }

# run CMD... with timeout, logging to $TMP/logs/<id>.log; sets CUR_RC, CUR_SECS, CUR_LOG
run() {
  local id="$1"; shift
  CUR_LOG="$TMP/logs/$id.log"
  local t0=$SECONDS
  echo "\$ $*" > "$CUR_LOG"
  if [ "$VERBOSE" = 1 ]; then
    ( cd "$LAB" && timeout -k 10 "$SCEN_TIMEOUT" "$@" ) 2>&1 | tee -a "$CUR_LOG"; CUR_RC=${PIPESTATUS[0]}
  else
    ( cd "$LAB" && timeout -k 10 "$SCEN_TIMEOUT" "$@" ) >> "$CUR_LOG" 2>&1; CUR_RC=$?
  fi
  CUR_SECS=$((SECONDS - t0))
}
tail_log() { echo "     --- last lines of $CUR_LOG ---"; tail -n 15 "$CUR_LOG" | sed 's/^/     | /'; }

# newest result dirs created by the last command(s): we wipe results/ before each launch scenario
result_dirs() { ls -1d "$LAB"/results/*/ 2>/dev/null; }
check_result_dir() { # dir expected_status -> echo problems
  local d="${1%/}" st="$2" p=""
  [ -f "$d/result.json" ] || { echo "no result.json in $(basename "$d")"; return; }
  local got; got="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("status"))' "$d/result.json" 2>&1)"
  [ "$got" = "$st" ] || p+="status=$got(want $st) "
  echo -n "$p"
}

scen_build() { # letter os
  local L="$1" os="$2" name="$1) build-image $2 --no-updates"
  echo "[$name]"
  [ -f "$LAB/build-image.sh" ] || { record "$name" SKIP "build-image.sh missing" 0; return; }
  run "$L-build-$os" bash "$LAB/build-image.sh" "$os" --no-updates
  local prob=""
  [ "$CUR_RC" = 0 ] || prob+="exit=$CUR_RC "
  [ -s "$WTL_IMAGES_DIR/$os.qcow2" ] || prob+="no images/$os.qcow2 "
  [ -f "$WTL_IMAGES_DIR/$os.json" ] || prob+="no images/$os.json "
  if [ -f "$WTL_IMAGES_DIR/$os.json" ]; then
    python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));assert str(d.get("os"))==sys.argv[2], d' \
      "$WTL_IMAGES_DIR/$os.json" "$os" 2>/dev/null || prob+="bad $os.json "
  fi
  [ -e "$WTL_IMAGES_DIR/$os.qcow2.partial" ] && prob+="leftover .partial "
  grep -q "BUILD_DONE" "$CUR_LOG" "$LAB"/runs/build-"$os"/serial.log 2>/dev/null || prob+="(no BUILD_DONE echoed) "
  if [ -z "$prob" ] || [ "$prob" = "(no BUILD_DONE echoed) " ]; then
    record "$name" PASS "${prob}" "$CUR_SECS"
  else
    record "$name" FAIL "$prob" "$CUR_SECS"; tail_log
  fi
  # make sure later launch scenarios have a base image even if the build failed
  if [ ! -s "$WTL_IMAGES_DIR/$os.qcow2" ]; then
    "$WTL_QEMU_IMG" create -q -f qcow2 "$WTL_IMAGES_DIR/$os.qcow2" 60G
    printf '{"os":"%s","iso":"fake","built_at":"fixture","updates":false}\n' "$os" > "$WTL_IMAGES_DIR/$os.json"
    echo "     (created fixture image $os.qcow2 so launch scenarios can run)"
  fi
}

# scen_launch letter name expect_rc expect_status|- [extra checks fn] -- launch args...
scen_launch() {
  local L="$1" name="$2" erc="$3" est="$4" chk="$5"; shift 5
  [ "${1:-}" = "--" ] && shift
  echo "[$L) $name]"
  [ -f "$LAB/launch-test.sh" ] || { record "$L) $name" SKIP "launch-test.sh missing" 0; return; }
  rm -rf "$LAB/results"
  local before; before="$(grep -c '' "$FAKE_QEMU_LOG")"
  run "$L-launch" bash "$LAB/launch-test.sh" "$@"
  local prob=""
  [ "$CUR_RC" = "$erc" ] || prob+="exit=$CUR_RC(want $erc) "
  [ "$CUR_RC" = 124 ] || [ "$CUR_RC" = 137 ] && prob+="(scenario timeout ${SCEN_TIMEOUT}s) "
  if [ "$est" != "-" ]; then
    local dirs; dirs="$(result_dirs)"
    if [ -z "$dirs" ]; then prob+="no results/<run> dir "; else
      local d; for d in $dirs; do prob+="$(check_result_dir "$d" "$est")"; done
    fi
  fi
  if [ -n "$chk" ]; then prob+="$($chk "$before")"; fi
  if [ -z "$prob" ]; then record "$L) $name" PASS "" "$CUR_SECS"
  else record "$L) $name" FAIL "$prob" "$CUR_SECS"; tail_log; fi
}

# extra checks
chk_pass_files() {
  local d p=""
  for d in $(result_dirs); do
    d="${d%/}"
    [ -s "$d/test.log" ] || p+="no test.log "
    grep -q "PASS" "$d/test.log" 2>/dev/null || p+="test.log lacks python-test output "
    [ -s "$d/screen.png" ] || p+="no screen.png "
    if [ -s "$d/screen.png" ]; then head -c 8 "$d/screen.png" | od -An -c | grep -q 'P   N   G' || p+="screen.png not PNG "; fi
    [ -f "$d/serial.log" ] || p+="no serial.log "
    [ -f "$d/python-test.json" ] || p+="no python-test.json (WTL_RESULTS not passed?) "
  done
  ls "$LAB"/runs/*/disk.qcow2 >/dev/null 2>&1 && p+="overlay not deleted "
  echo -n "$p"
}
chk_hostport() { # host port was used in the qemu argv
  local before="$1"
  tail -n +"$((before + 1))" "$FAKE_QEMU_LOG" | grep -q "hostfwd=tcp:127.0.0.1:$RPORT-:5000" \
    || echo -n "argv lacks hostfwd=tcp:127.0.0.1:$RPORT-:5000 "
  grep -q "\"port\": $RPORT" "$(result_dirs | head -1)python-test.json" 2>/dev/null \
    || echo -n "test did not use WTL_PORT=$RPORT "
}
chk_no_qemu() {
  local before="$1"
  tail -n +"$((before + 1))" "$FAKE_QEMU_LOG" | grep '"argv"' | grep -v 'qemu-img' | grep -q . \
    && echo -n "qemu was started "
  [ "$CUR_SECS" -le 15 ] || echo -n "took ${CUR_SECS}s (not quick) "
  tail -n +2 "$CUR_LOG" | grep -qi -E 'program\.exe|not found|not in|missing' || echo -n "no clear message "
}
chk_summary() {
  local p="" n; n="$(result_dirs | wc -l)"
  [ "$n" -ge 2 ] || p+="only $n result dirs "
  ls -d "$LAB"/results/*-2012* >/dev/null 2>&1 || p+="no 2012 run "
  ls -d "$LAB"/results/*-2016* >/dev/null 2>&1 || p+="no 2016 run "
  tail -n +2 "$CUR_LOG" | grep -q 2012 && tail -n +2 "$CUR_LOG" | grep -q 2016 || p+="summary lacks os names "
  grep -qi -E 'summary|pass' "$CUR_LOG" || p+="no summary table "
  echo -n "$p"
}
chk_helpful() {
  tail -n +2 "$CUR_LOG" | grep -qi -E 'build-image|no base image|not found|missing|does not exist' || echo -n "no helpful message "
}

# ---------------------------------------------------------------- scenarios
echo "run-tests: temp lab $LAB (FAKE_QEMU_SPEED=$FAKE_QEMU_SPEED, per-scenario timeout ${SCEN_TIMEOUT}s)"
want a || want c || want d || want e || want f || want g || want i || want k || want l || want m || want n || want o || want p && scen_build a 2012
want b || want i && scen_build b 2016
# a UEFI build (Win11): the fake waits for a key at "press any key to boot from CD", like OVMF
want q && scen_build q win11

want c && scen_launch c "launch demo program.bat 2012 --port 5000" 0 pass chk_pass_files -- \
  "$DEMO_ZIP" program.bat 2012 --port 5000 --test "$PYTEST" --timeout 90

if want d; then
  RPORT="$(free_port)"
  scen_launch d "launch --port 5000:$RPORT (random host port)" 0 pass chk_hostport -- \
    "$DEMO_ZIP" program.bat 2012 --port "5000:$RPORT" --test "$PYTEST" --timeout 90
fi

want e && scen_launch e "program crashes before READY -> exit 2" 2 - "" -- \
  "$TMP/data/crash.zip" program.bat 2012 --port 5000 --test "$PYTEST" --timeout 60

want f && scen_launch f "wrong protocol replies -> exit 1" 1 fail "" -- \
  "$TMP/data/wrong.zip" program.bat 2012 --port 5000 --test "$PYTEST" --timeout 60

want g && scen_launch g "program hangs, --timeout 20 -> exit 3" 3 timeout "" -- \
  "$TMP/data/hang.zip" program.bat 2012 --port 5000 --test "$PYTEST" --timeout 20

want h && scen_launch h "exe not in zip -> exit 2, qemu not started" 2 - chk_no_qemu -- \
  "$DEMO_ZIP" program.exe 2012 --port 5000 --test "$PYTEST"

want i && scen_launch i "os all -> 2012 + 2016 + summary" 0 pass chk_summary -- \
  "$DEMO_ZIP" program.bat all --port 5000 --test "$PYTEST" --timeout 90

if want j; then
  EMPTY="$TMP/no-images"; mkdir -p "$EMPTY"
  WTL_IMAGES_DIR="$EMPTY" scen_launch j "missing base image -> exit 2 + message" 2 - chk_helpful -- \
    "$DEMO_ZIP" program.bat 2012 --port 5000 --test "$PYTEST"
fi

# ---- exit mode (no --port): program runs to completion, stdout + exit code captured
FETCH_OUT='fetch-ria 0.1.0\r\nwindows: 6.3.9600\r\nurl: https://www.ria.ee/\r\nattempt: 1\r\nstatus: 200\r\nbytes: 61234\r\ntitle: RIA\r\nRESULT OK\r\n'
behavior_zip "{\"stdout\":\"$FETCH_OUT\",\"code\":0}" "$TMP/data/exit-ok.zip"
behavior_zip '{"stdout":"fetch-ria 0.1.0\r\nerror: WinHttpSendRequest failed: 12175\r\nRESULT FAIL network\r\n","code":2}' "$TMP/data/exit-fail.zip"
behavior_zip '{"stdout":"still going\r\n","sleep":30}' "$TMP/data/exit-hang.zip"
python3 - "$TMP/data/big.json" <<'PY'
import json, sys
json.dump({"stdout": "".join("line %05d some program output\r\n" % i for i in range(3000)), "stderr": "warn: x\r\n"}, open(sys.argv[1], "w"))
PY
d="$TMP/data/beh-big"; mkdir -p "$d"; cp "$TMP/data/big.json" "$d/fake-behavior.json"
bash "$ROOT/examples/make-demo-zip.sh" "$TMP/data/exit-big.zip" "$d/fake-behavior.json" >/dev/null
FETCHTEST="$ROOT/examples/fetch-test.py"

chk_exit_files() { # stdout.txt has RESULT OK, result.json mode=exit, exit_code=0
  local d p=""
  for d in $(result_dirs); do
    d="${d%/}"
    grep -q "RESULT OK" "$d/stdout.txt" 2>/dev/null || p+="stdout.txt lacks RESULT OK "
    python3 -c 'import json,sys;r=json.load(open(sys.argv[1]));assert r["mode"]=="exit" and r["program"]["exit_code"]==0, r' \
      "$d/result.json" 2>/dev/null || p+="result.json mode/exit_code wrong "
    grep -q "WTL OUTPUT " "$d/result.json" && p+="OUTPUT chunks in result.json events "
  done
  echo -n "$p"
}
chk_fetch_json() {
  local d; d="$(result_dirs | head -1)"
  python3 -c 'import json,sys;r=json.load(open(sys.argv[1]));assert r["passed"] and r["windows"]=="6.3.9600", r' \
    "${d}fetch-test.json" 2>/dev/null || echo -n "fetch-test.json missing/wrong "
}
chk_big() {
  local d; d="$(result_dirs | head -1)"
  python3 - "${d}stdout.txt" "${d}stderr.txt" <<'PY' || echo -n "big stdout not reassembled exactly "
import sys
want = "".join("line %05d some program output\r\n" % i for i in range(3000)).encode()
assert open(sys.argv[1], "rb").read() == want
assert open(sys.argv[2], "rb").read() == b"warn: x\r\n"
PY
}

want k && scen_launch k "exit mode: program exits 0, no --test -> pass" 0 pass chk_exit_files -- \
  "$TMP/data/exit-ok.zip" program.bat 2012 --timeout 90
want l && scen_launch l "exit mode + fetch-test.py -> pass" 0 pass chk_fetch_json -- \
  "$TMP/data/exit-ok.zip" program.bat 2012 --test "$FETCHTEST" --timeout 90
want m && scen_launch m "exit mode: exit 2 + fetch-test.py -> fail" 1 fail "" -- \
  "$TMP/data/exit-fail.zip" program.bat 2012 --test "$FETCHTEST" --timeout 90
want n && scen_launch n "exit mode: exit 2, --expect-exit 2 -> pass" 0 pass "" -- \
  "$TMP/data/exit-fail.zip" program.bat 2012 --expect-exit 2 --timeout 90
want o && scen_launch o "exit mode: runs past --run-timeout 5 -> exit 3" 3 timeout "" -- \
  "$TMP/data/exit-hang.zip" program.bat 2012 --run-timeout 5 --timeout 90
want p && scen_launch p "exit mode: 100 KB stdout reassembled byte-exact" 0 pass chk_big -- \
  "$TMP/data/exit-big.zip" program.bat 2012 --timeout 90

# leaked fake-qemu processes?
sleep 1
LEAKS=""
for f in "$PIDDIR"/*; do
  [ -e "$f" ] || continue
  p="$(basename "$f")"; kill -0 "$p" 2>/dev/null && LEAKS+="$p "
done
echo "[z) no fake-qemu left running]"
if [ -z "$LEAKS" ]; then record "z) no fake-qemu left running" PASS "" 0
else record "z) no fake-qemu left running" FAIL "leaked pids: $LEAKS" 0; fi

# ---------------------------------------------------------------- report
echo
printf '%-58s %-5s %5s  %s\n' "SCENARIO" "RES" "SECS" "NOTES"
printf '%-58s %-5s %5s  %s\n' "--------" "---" "----" "-----"
FAILS=0; PASSES=0; SKIPS=0
for i in "${!NAMES[@]}"; do
  printf '%-58s %-5s %5s  %s\n' "${NAMES[$i]}" "${RESULTS[$i]}" "${TIMES[$i]}" "${NOTES[$i]}"
  case "${RESULTS[$i]}" in PASS) PASSES=$((PASSES+1));; FAIL) FAILS=$((FAILS+1));; *) SKIPS=$((SKIPS+1));; esac
done
echo
echo "run-tests: $PASSES passed, $FAILS failed, $SKIPS skipped (logs: $TMP/logs$( [ "$KEEP_TMP" = 1 ] || echo ', use -k to keep'))"
[ "$FAILS" -eq 0 ]
