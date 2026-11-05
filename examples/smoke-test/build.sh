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
# Windows Server 2003 exists here only as a 32-bit image: out32/ holds the same program built for
# it (needs gcc-mingw-w64-i686; skipped without it) - see examples/configs/single-2003-smoke.json
if command -v i686-w64-mingw32-gcc >/dev/null 2>&1; then
  od32="$here/out32"
  rm -rf "$od32"; mkdir -p "$od32"
  i686-w64-mingw32-gcc -O2 -static-libgcc -D_WIN32_WINNT=0x0502 -o "$od32/smoke.exe" "$here/smoke.c"
  cp "$here/setup.bat" "$od32/"
  echo "built $od32/ (32-bit, for Windows Server 2003)"
else
  echo "skipped out32/ (no i686-w64-mingw32-gcc; sudo apt-get install gcc-mingw-w64-i686)"
fi
