#!/usr/bin/env bash
# tests/all.sh - run every win-test-lab test that needs no VM / no Windows.
#   tests/all.sh
# Covers: PowerShell lint, the run-config parser unit tests, a bash syntax check of the
# scripts, and the fake-qemu host-logic suite. Exit 0 = all green.
set -uo pipefail
TESTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TESTS/.." && pwd)"
cd "$ROOT"
rc=0
step() { echo; echo "=== $* ==="; }

step "bash syntax"
for s in lab.sh build-image.sh launch-test.sh; do
    [ -f "$s" ] || continue
    if bash -n "$s"; then echo "ok  $s"; else echo "FAIL $s"; rc=1; fi
done

step "PowerShell lint (guest/*.ps1)"
if python3 tests/ps_lint.py guest/*.ps1; then :; else rc=1; fi

step "run-config parser (tests/test_labconf.py)"
if python3 tests/test_labconf.py; then :; else rc=1; fi

step "example configs validate"
for c in examples/configs/*.conf; do
    [ -f "$c" ] || continue
    if python3 lib/labconf.py check "$c" >/dev/null; then echo "ok  $c"; else echo "FAIL $c"; rc=1; fi
done

step "lab definitions resolve (tests/check-labs.sh)"
if bash tests/check-labs.sh; then :; else rc=1; fi

step "image catalog (images-list.conf) resolves to present ISOs"
if bash tests/check-catalog.sh; then :; else rc=1; fi

step "fake-qemu host-logic suite (tests/run-tests.sh)"
if bash tests/run-tests.sh; then :; else rc=1; fi

echo
if [ "$rc" = 0 ]; then echo "ALL TESTS PASSED"; else echo "SOME TESTS FAILED"; fi
exit "$rc"
