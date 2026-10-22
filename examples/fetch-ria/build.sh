#!/usr/bin/env bash
# build.sh - cross-compile fetch-ria.exe on Linux and pack it as fetch-ria.zip
#
#   examples/fetch-ria/build.sh            -> examples/fetch-ria/fetch-ria.zip (contains fetch-ria.exe)
#
# Needs (one time):
#   curl https://sh.rustup.rs -sSf | sh            # Rust, if you don't have it
#   sudo apt-get install gcc-mingw-w64-x86-64      # Windows linker
# The script adds the rust target x86_64-pc-windows-gnu itself.
#
# Toolchain and old Windows: Rust 1.78+ officially needs Windows 10+. Exes built with it
# often still run on Server 2012 R2 (Windows 8.1 kernel) but are not guaranteed to;
# that's what testing on 2012 is for. To use an older toolchain that still targets Windows 7/8.1:
#   RUST_TOOLCHAIN=1.77.2 examples/fetch-ria/build.sh
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target=x86_64-pc-windows-gnu
tc="${RUST_TOOLCHAIN:-}"
plus=()
[[ -n "$tc" ]] && plus=("+$tc")

command -v cargo >/dev/null || { echo "cargo not found - install Rust: curl https://sh.rustup.rs -sSf | sh" >&2; exit 2; }
command -v x86_64-w64-mingw32-gcc >/dev/null || {
    echo "x86_64-w64-mingw32-gcc not found - sudo apt-get install gcc-mingw-w64-x86-64" >&2; exit 2; }
if command -v rustup >/dev/null; then
    [[ -n "$tc" ]] && rustup toolchain install --profile minimal "$tc"
    rustup target add ${tc:+--toolchain "$tc"} "$target"
fi

export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
cargo ${plus[@]+"${plus[@]}"} build --release --target "$target" --manifest-path "$here/Cargo.toml" \
    --target-dir "$here/target"
exe="$here/target/$target/release/fetch-ria.exe"
[[ -f "$exe" ]] || { echo "build produced no $exe" >&2; exit 2; }

# show which DLLs the exe needs (all should be Windows system DLLs)
if command -v x86_64-w64-mingw32-objdump >/dev/null; then
    echo "DLL imports:"
    x86_64-w64-mingw32-objdump -p "$exe" | sed -n 's/^\s*DLL Name: /    /p'
fi

python3 - "$exe" "$here/fetch-ria.zip" <<'PY'
import sys, zipfile
exe, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    z.write(exe, "fetch-ria.exe")
print("wrote", out)
PY
