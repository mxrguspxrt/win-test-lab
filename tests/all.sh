#!/usr/bin/env bash
# tests/all.sh - run every win-test-lab test that needs no VM / no Windows.
#   tests/all.sh
# Covers: `wtl` (cargo test: unit tests and the fake-QEMU lab suite), clippy and fmt,
# PowerShell lint of the guest scripts, and the labs and example run configs. Exit 0 = all green.
set -uo pipefail
TESTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TESTS/.." && pwd)"
cd "$ROOT"
rc=0
step() { echo; echo "=== $* ==="; }

step "wtl (cargo test)"
if cargo test --quiet 2>&1 | grep -E "test result|scenarios|FAIL|error"; then :; fi
cargo test --quiet >/dev/null 2>&1 || { echo "FAIL cargo test"; rc=1; }

step "clippy + fmt"
if cargo clippy --all-targets --quiet -- -D warnings; then echo "ok  clippy"; else echo "FAIL clippy"; rc=1; fi
if cargo fmt --check >/dev/null; then echo "ok  fmt"; else echo "FAIL fmt (run cargo fmt)"; rc=1; fi

step "the wtl crate (wtl/: cargo install wtl) builds against this library"
if (cd wtl && cargo build --quiet && cargo clippy --quiet -- -D warnings); then echo "ok  wtl/"; else echo "FAIL wtl/"; rc=1; fi

step "PowerShell lint (guest/*.ps1)"
if command -v python3 >/dev/null; then
    if python3 tests/ps_lint.py guest/*.ps1; then :; else rc=1; fi
else
    echo "SKIP python3 not found"
fi

step "labs and example run configs resolve (tests/check-labs.sh)"
if bash tests/check-labs.sh; then :; else rc=1; fi

echo
if [ "$rc" = 0 ]; then echo "ALL TESTS PASSED"; else echo "SOME TESTS FAILED"; fi
exit "$rc"
