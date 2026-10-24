#!/bin/sh
# build.sh - build smoke.exe for x64 and Arm64 Windows with only rustup (any host: Linux,
# macOS, Windows; no Windows SDK, no MinGW):
#   examples/smoke-rs/out/smoke-x64.exe    examples/smoke-rs/out/smoke-arm64.exe
# plus setup.bat, so a run config can mount out/ as X: (examples/configs/*-arm64-smoke.json).
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc >/dev/null 2>&1 || true
mkdir -p "$here/out"
cd "$here"   # cargo reads .cargo/config.toml (the rust-lld linker) from the current directory
for t in x86_64-pc-windows-msvc:x64 aarch64-pc-windows-msvc:arm64; do
    cargo build --release --quiet --target "${t%%:*}"
    cp "$here/target/${t%%:*}/release/smoke.exe" "$here/out/smoke-${t##*:}.exe"
done
cp "$here/../smoke-test/setup.bat" "$here/out/"
echo "built $here/out/: $(cd "$here/out" && echo *)"
