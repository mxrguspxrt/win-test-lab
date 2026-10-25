#!/usr/bin/env bash
# build.sh - cross-compile the standalone smoke.exe example (needs gcc-mingw-w64-x86-64).
# Produces examples/smoke-test/out/ (smoke.exe + setup.bat), the folder the example run configs
# mount as X: (see examples/configs/school-smoke.json). smoke.exe is self-contained - no DLLs,
# no data files.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
od="$here/out"
rm -rf "$od"; mkdir -p "$od"
x86_64-w64-mingw32-gcc -O2 -o "$od/smoke.exe" "$here/smoke.c"
cp "$here/setup.bat" "$od/"
echo "built $od/"
