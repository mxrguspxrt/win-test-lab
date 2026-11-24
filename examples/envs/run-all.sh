#!/usr/bin/env bash
# run-all.sh - environments end to end on real VMs, every result checked; stops at the first
# failure. The same commands as README.md, one after the other.
#
#   examples/envs/build.sh                    # once: out/whereami.exe (and out32/ for 2003)
#   examples/envs/run-all.sh single-2019      # or single-2003, single-2022, ... or 2019 (AD)
#
# It makes the environment LAB-demo, runs whereami.exe in it and in LAB itself at the same time
# (each must know which one it is), makes it fresh again, and deletes it. LAB is restored at the
# end (its own VMs are stopped). WTL=path/to/wtl uses another wtl than the one on the PATH.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
wtl=${WTL:-wtl}
lab=${1:-}
case $lab in
  single-2003) cfg=$here/whereami-2003.json; exe=$here/out32/whereami.exe ;;
  2019) cfg=$here/whereami-ad.json; exe=$here/out/whereami.exe ;;
  single-*) cfg=$here/whereami.json; exe=$here/out/whereami.exe ;;
  *) echo "usage: run-all.sh LAB    (a single-<os> lab, e.g. single-2019, or 2019 for the AD lab)"; exit 2 ;;
esac
env=$lab-demo
logs=$(mktemp -d "${TMPDIR:-/tmp}/wtl-envs-XXXXXX")
step() { echo; echo "=== $*"; }
fail() { echo; echo "FAIL: $*"; echo "(logs: $logs)"; exit 1; }
id_of() { "$wtl" list | awk -v n="$1" '$1 == n { print $3 }'; }
last_run() { "$wtl" status "$1" | sed -n 's/^  last run: //p'; }

# whereami.exe must say RESULT OK in every step of the run behind LOG, and result.json must
# carry the expected environment and id
check_run() { # NAME ID LOG
  local name=$1 id=$2 log=$3 res steps oks
  grep -q 'ALL STEPS OK' "$log" || fail "the run in $name did not pass (see $log)"
  res=$(last_run "$name")
  [ -f "$res/result.json" ] || fail "$name: no result.json"
  grep -q "\"environment\": \"$name\"" "$res/result.json" || fail "$name: result.json names another environment"
  grep -q "\"id\": \"$id\"" "$res/result.json" || fail "$name: result.json has another id"
  grep -q "\"run_id\": \"[0-9-]*-$id\"" "$res/result.json" || fail "$name: result.json's run_id does not end in $id"
  steps=$(ls "$res"/*/step*out.txt | wc -l)
  oks=$(cat "$res"/*/step*out.txt | grep -c '^RESULT OK')
  for f in "$res"/*/step*out.txt; do echo "  $(basename "$(dirname "$f")") $(basename "$f"): $(grep '^RESULT' "$f")"; done
  [ "$steps" -gt 0 ] && [ "$oks" -eq "$steps" ] || fail "$name: $oks of $steps steps said RESULT OK"
}

[ -f "$exe" ] || fail "no $exe: run examples/envs/build.sh first"

step "1. a new environment of $lab: $env"
"$wtl" rm "$env" > /dev/null 2>&1
"$wtl" up "$lab" --new --name demo --note "whereami demo" > "$logs/up.log" 2>&1 || fail "wtl up $lab --new failed (see $logs/up.log)"
grep "environment $env (id" "$logs/up.log" | head -1
grep -A20 "^  environment $env" "$logs/up.log"

step "2. wtl list: $env is there, running, with its note"
"$wtl" list | tee "$logs/list.log"
id=$(id_of "$env")
lab_id=$(id_of "$lab")
[ ${#id} -eq 8 ] || fail "no id for $env in wtl list"
[ ${#lab_id} -eq 8 ] || fail "no id for $lab in wtl list"
[ "$id" != "$lab_id" ] || fail "$env and $lab have the same id"
grep -q "^$env .* $lab .* $id .* running .*whereami demo" "$logs/list.log" || fail "wtl list does not show $env running with its note"

step "3. wtl status: the environment, and one machine of it"
machine=$("$wtl" status "$lab" | awk '$2 == "single" || $2 == "member" { print $1; exit }')
"$wtl" status "$env" | tee "$logs/status.log"
grep -q "environment $env (id $id) of lab $lab" "$logs/status.log" || fail "wtl status $env does not say what it is"
"$wtl" status "$env/$machine" | tee "$logs/status-machine.log"
grep -q "^  $machine " "$logs/status-machine.log" || fail "wtl status $env/$machine does not show $machine"

# SYSTEM steps see the step variables only from a 0 state built by wtl 0.3+ (its SYSTEM worker)
state_dir=$("$wtl" home | awk '$1 == "state_dir" { print $2 }')
sysvars=""
grep -q '"step-env"' "$state_dir/$lab/0-state.json" 2>/dev/null && sysvars="--vars"

step "4. whereami.exe in $env (id $id) and in $lab (id $lab_id), at the same time"
WTL_EXPECT_ENV=$env WTL_EXPECT_ID=$id WTL_SYSTEM_VARS=$sysvars "$wtl" run "$cfg" --lab "$env" > "$logs/run-env.log" 2>&1 &
a=$!
WTL_EXPECT_ENV=$lab WTL_EXPECT_ID=$lab_id WTL_SYSTEM_VARS=$sysvars "$wtl" run "$cfg" --lab "$lab" > "$logs/run-lab.log" 2>&1 &
b=$!
wait $a
wait $b
echo "in $env:"
check_run "$env" "$id" "$logs/run-env.log"
echo "in $lab:"
check_run "$lab" "$lab_id" "$logs/run-lab.log"

step "5. wtl rebuild $env: fresh again from $lab's 0 state, same id"
"$wtl" rebuild "$env" | tee "$logs/rebuild.log"
[ "$(id_of "$env")" = "$id" ] || fail "$env has another id after wtl rebuild"

step "6. wtl rm $env"
"$wtl" rm "$env" | tee "$logs/rm.log"
[ -z "$(id_of "$env")" ] || fail "$env is still in wtl list"
"$wtl" status "$env" > /dev/null 2>&1 && fail "wtl status $env still works after wtl rm"
"$wtl" restore "$lab" > /dev/null 2>&1

echo
echo "ALL CHECKS PASSED for $lab (logs: $logs)"
