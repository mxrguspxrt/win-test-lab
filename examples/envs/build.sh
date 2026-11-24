#!/usr/bin/env bash
# build.sh - cross-compile whereami.exe (needs gcc-mingw-w64-x86-64). Produces out/whereami.exe,
# the folder the run files here mount as X:, and with gcc-mingw-w64-i686 also out32/ (32-bit,
# for Windows Server 2003). One self-contained .exe: no DLLs, no data files.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
od="$here/out"
rm -rf "$od"; mkdir -p "$od"
x86_64-w64-mingw32-gcc -O2 -Wall -o "$od/whereami.exe" "$here/whereami.c"
echo "built $od/whereami.exe"
if command -v i686-w64-mingw32-gcc >/dev/null 2>&1; then
  od32="$here/out32"
  rm -rf "$od32"; mkdir -p "$od32"
  i686-w64-mingw32-gcc -O2 -Wall -static-libgcc -D_WIN32_WINNT=0x0502 -o "$od32/whereami.exe" "$here/whereami.c"
  echo "built $od32/whereami.exe (32-bit, for Windows Server 2003)"
else
  echo "skipped out32/ (no i686-w64-mingw32-gcc; sudo apt-get install gcc-mingw-w64-i686)"
fi
