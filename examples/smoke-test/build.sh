#!/usr/bin/env bash
# build.sh - cross-compile the standalone smoke.exe example (needs gcc-mingw-w64-x86-64).
# Produces two things:
#   examples/smoke-test/out/         (the AD lab config mounts it: mount = ../smoke-test/out:X,
#                                     see examples/configs/2019-smoke.conf)
#   examples/smoke-test/smoke.zip    (for the single-VM launch-test.sh: launch-test.sh smoke.zip smoke.exe 2012)
# smoke.exe is self-contained - no DLLs, no data files.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cc=x86_64-w64-mingw32-gcc
"$cc" -O2 -o "$tmp/smoke.exe" "$here/smoke.c"

# smoke.zip (just smoke.exe)
rm -f "$here/smoke.zip"
(cd "$tmp" && python3 -c 'import sys,zipfile
z=zipfile.ZipFile(sys.argv[1],"w",zipfile.ZIP_DEFLATED)
z.write("smoke.exe","smoke.exe")
z.close()' "$here/smoke.zip")
echo "built $here/smoke.zip"

# out/ - the folder a config mounts as a drive (smoke.exe + setup.bat)
od="$here/out"
rm -rf "$od"; mkdir -p "$od"
cp "$tmp/smoke.exe" "$here/setup.bat" "$od/"
echo "built $od/"
