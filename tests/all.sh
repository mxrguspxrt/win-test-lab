#!/usr/bin/env bash
# tests/all.sh - run every win-test-lab test that needs no VM / no Windows.
#   tests/all.sh
# Covers: the Rust `lab` (cargo test: unit tests and the fake-QEMU lab suite), PowerShell
# lint, a bash syntax check of the scripts, the labs and example run configs, and the
# fake-qemu suite for build-image.sh / launch-test.sh. Exit 0 = all green.
set -uo pipefail
TESTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TESTS/.." && pwd)"
cd "$ROOT"
rc=0
step() { echo; echo "=== $* ==="; }

step "Rust lab (cargo test in wtl-rs/)"
if command -v cargo >/dev/null; then
    if (cd wtl-rs && cargo test --quiet 2>&1 | grep -E "test result|scenarios|FAIL|error" ); then :; fi
    (cd wtl-rs && cargo test --quiet >/dev/null 2>&1) || { echo "FAIL cargo test"; rc=1; }
else
    echo "SKIP cargo not found"
fi

step "bash syntax"
for s in build-image.sh launch-test.sh download-isos.sh setup-host.sh lib/common.sh lib/rename-isos.sh; do
    [ -f "$s" ] || continue
    if bash -n "$s"; then echo "ok  $s"; else echo "FAIL $s"; rc=1; fi
done

step "PowerShell lint (guest/*.ps1)"
if python3 tests/ps_lint.py guest/*.ps1; then :; else rc=1; fi

step "labs and example run configs resolve (tests/check-labs.sh)"
if bash tests/check-labs.sh; then :; else rc=1; fi

step "image catalog (images-list.conf) resolves to present ISOs"
if bash tests/check-catalog.sh; then :; else rc=1; fi

step "fake-qemu suite for build-image.sh / launch-test.sh (tests/run-tests.sh)"
if bash tests/run-tests.sh; then :; else rc=1; fi

echo
if [ "$rc" = 0 ]; then echo "ALL TESTS PASSED"; else echo "SOME TESTS FAILED"; fi
exit "$rc"
